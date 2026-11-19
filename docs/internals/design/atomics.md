# Atomics

Status: partly implemented. Issue: [#708](https://github.com/velt-lang/velt/issues/708).

## Summary

`Atomic<T>` is a word in place that threads read and change at once, without a lock. Each method
compiles to the one instruction (or short compare-and-swap loop) Rust's `core::sync::atomic`
emits for it, with the memory ordering the call names. On it, std builds the lock-free
structures in Velt itself: a version latch for optimistic lock coupling, epoch-based
reclamation, `Atom<T>` (a replaceable shared value, like Rust's `arc-swap`) and a concurrent
B+ tree map.

## Motivation

A lock (`Mutex<T>`, `RwLock<T>`) and the atomic methods of `shared<i64>` (`add`, `get`, `set`)
cannot express the techniques the fastest concurrent data structures use:

- **Optimistic lock coupling**: a 64-bit latch word per node, read with an acquire load, checked
  after the reads with an acquire fence and a relaxed load, taken with a compare-and-swap that
  never waits, and let go with a release store that bumps the version. A reader takes no lock and
  writes no shared cache line.
- **Seqlocks, counters, flags, ticket and MCS locks, work-stealing deques, epochs**: all a few
  words that are loaded, stored, swapped and compared-and-swapped with the weakest ordering that
  is correct.

These need a word in every node, in place, with acquire and release orderings that are a plain
`mov` on x86 and `ldapr`/`stlr` on arm64.

## Design

### A word in place

`Atomic<T>` is a prelude struct (`std/prelude/atomic.velt`) for `T` an integer type, `f32`,
`f64`, `bool`, `string`, `shared<U>` or `shared<U> | null`. A number or `bool` is laid out as a
plain `T` (no count, no box) wherever it is stored: a local, a field of a class or struct, an
element of an array. Its methods are the only way to read or write it.

**It is the one part of a `shared<T>` that changes.** A `shared<T>` never changes, and a frozen
read reaches its fields and elements in place, in its box. An `Atomic` reached that way is changed
in place: `s.nodes[i].version.store(v + 1, "release")` writes the word in the box. That is sound
because a number's `Atomic` holds no count and no pointer: it is a number every thread may read
and write with atomic instructions.

**An `Atomic` is never copied or shared.** Copying the word would split one atomic into two
values, and sharing one would make lowering count the type, which boxes it and breaks the "word in
place" layout. So:

```text
const v = node.version;     // error: an `Atomic<u64>` cannot be copied or shared
f(node.version);            // the same, unless `f`'s parameter is borrowed
return node.version;        // the same
```

An `Atomic` is moved into its place when that place is made (`new Atomic<u64>(0)` in a field
initializer, a struct literal or `Array.from`), as a `Mutex` is moved into `shared(...)`.

**Arrays of atomics are type-stable memory.** An array inside a `shared<T>` never grows, shrinks
or moves while the box lives, so a `shared<Pool>` with `Atomic<u64>[]` fields is a block of words
every thread may read and write in place and no thread can free under another.

### API

A summary of the prelude's declarations (`std/prelude/atomic.velt` spells each ordering union
out):

```text
type LoadOrder = "relaxed" | "acquire" | "seqcst";
type StoreOrder = "relaxed" | "release" | "seqcst";
type Order = "relaxed" | "acquire" | "release" | "acqrel" | "seqcst";

struct Atomic<T> {
  new Atomic<T>(v: T)
  load(order: LoadOrder = "seqcst"): T
  store(v: T, order: StoreOrder = "seqcst"): void
  exchange(v: T, order: Order = "seqcst"): T                  // the previous value
  compareExchange(expected: T, v: T, order: Order = "seqcst",
                  failure: LoadOrder = <from order>): T       // the previous value
  compareExchangeWeak(expected: T, v: T, order?, failure?): bool  // may fail spuriously
  add(v: T, order?): T   sub(…): T   and(…): T   or(…): T   xor(…): T   // previous value
  max(v: T, order?): T   min(v: T, order?): T
  static fence(order: Exclude<Order, "relaxed"> = "seqcst"): void
  static spinHint(): void                    // `pause` / `isb`, for spin loops
}
```

- **Names follow JavaScript's `Atomics`** (`load`, `store`, `exchange`, `compareExchange`, `add`,
  `sub`, `and`, `or`, `xor`), and so do the results: the read-modify-write methods return the
  *previous* value, as `Atomics.add` and Rust's `fetch_add` do. `compareExchangeWeak`, `max`,
  `min`, the orderings, `fence` and `spinHint` are additions, as in Rust.
- **Orderings are string literal types**, default `"seqcst"` (`Atomics`' only ordering). An
  ordering must be a literal, because the instruction is chosen at compile time, and an ordering
  the operation cannot have is a type error (`load("release")`), as in C++ and Rust. A `failure`
  ordering defaults from `order` as Rust's `compare_exchange` would pick it (`acqrel` →
  `acquire`, `release` → `relaxed`).
- `add`, `sub`, `max` and `min` need a number; `and`, `or` and `xor` need an integer or `bool`.
- **`shared<i64>`'s `add`/`get`/`set`** compile through the same inline path. Their `add` returns
  the *new* value; they are a different API.

### Floats

`Atomic<f64>` and `Atomic<f32>` are the words of their bits (`u64`, `u32`). `load`, `store`,
`exchange` and the compare-and-swaps are the word's. `add`, `sub`, `max` and `min` are a strong
compare-and-swap loop: a relaxed load, the operation on the float, and a swap with the call's
ordering whose relaxed failure feeds the next try, as Rust's `fetch_update` and the atomic float
crates do. The loop compares words, so a NaN value is no reason to spin. `max` and `min` are
`Math.max` and `Math.min`.

### References

`Atomic<shared<T>>` and `Atomic<shared<T> | null>`: the word is the pointer of one counted
reference that the `Atomic` owns. It is a field, so the holder's drop glue releases it; null is
none.

- **`load`** is uncounted: the reference is *borrowed*. Read in place (`a.load().f`, a borrowing
  argument) or bound by a `const`, it costs the load alone; a consumer that keeps it counts it, as
  for any borrowed place. The function runs in an epoch section (below), so a borrow stays valid
  until it returns. In an async function or a generator, `load` counts at once instead, inside a
  section of its own, and a `const` binds the value.
- **`store(v)` and `exchange(v)`** move `v` in by an atomic exchange; `exchange` gives the caller
  a counted copy of the old reference.
- **`compareExchange(expected, v)`** compares the pointers (identity) and returns whether it
  swapped, as `bool`; `v` is dropped when it did not. `expected` is only read, and its owner keeps
  it alive, so its address cannot be reused meanwhile (no ABA).
- **Retiring:** the reference a store, an exchange or a successful compare-and-swap takes out of
  the word is *retired*, not released, because another thread may have loaded the pointer and not
  yet counted it. Lowering hands it to `velt/sync/epoch`'s `retire`, which drops it once no
  thread in a section can still read it.
- **`a.load()!` and `a.load() ?? d`** bind the loaded value by borrow, so a consumer that keeps
  it (`const o = a.load()!`, a return) counts it.

### Strings

A string is three words, so an `Atomic<string>`'s word points to a counted box holding one, the
box a `shared<string>` has. `new Atomic<string>(s)` boxes `s`. `load` gives the string in the box,
borrowed (copied at once in an async function or a generator); `store` and `exchange` box the new
string and retire the old box; `exchange` gives a copy of the string before. `compareExchange`
compares *contents*: it loads the box (acquire), compares its string with `expected`, and swaps
the pointer only if no writer replaced the box meanwhile; a strong one reads again when another
writer did, a weak one fails. It returns `bool` and drops `v`'s box when it did not swap.

### Generic code

`Atomic<T>` with `T` a type parameter is allowed when `T` is bounded by the prelude's marker
interface `AtomicValue`. The compiler knows its implementations (no `extend` declares them): the
types listed above. The bound is checked where the generic is instantiated, and lowering picks the
instruction per instance. On such a `T` only the operations every `AtomicValue` has alike are
allowed: `load`, `store`, `exchange` and `compareExchangeWeak` (`compareExchange` returns the
value before for a number but whether it swapped for a reference, and the arithmetic and bitwise
methods exist for numbers only). A `const c = a.load()` there binds by value, since sema cannot
tell a reference load from a number's.

The prelude's marker interface `Frozen` is compiler-known the same way: it holds for the types
whose `shared` values are read in place without a lock or a copy (what holds no `Mutex`, weak
reference, promise, function or interface value, and no class with subclasses or a `clone()` of
its own). A type parameter bounded by it lets generic code read a `shared<V>` as a `V`;
`ConcurrentMap<K, V>` stores a non-`u64` value as `shared<V>` and returns it this way.

### Module constants

A module-level `const` whose type holds an `Atomic` is kept in the module state (made once), as a
non-constant initializer is: evaluated at each use, every use would change a new atomic. Reaching
a slot takes the module lock, so tasks using it would run one at a time. A **static atomic**,
`const X = new Atomic<T>(literal)`, is therefore not kept there: its word is writable data in the
program, starting as the literal, and its methods act on it directly from every thread, with no
lock, also in std. Every version in a `velt dev` session uses the session's one word per
constant, so a swap keeps its value. `new Atomic<shared<T> | null>(null)` is a static atomic too;
at exit the runtime calls a function `velt_main` hands it that releases the references such words
hold, for the leak counters. A static `Atomic<string>` stays in the module state, since its box is
made at run time.

### Built on atomics, in Velt

| Module | What |
|---|---|
| `velt/sync` `Backoff` | spins with `spinHint`, twice as long each round, then yields the thread |
| `velt/sync` `VersionLatch` | a latch word for optimistic lock coupling (`readVersion`, `tryReadVersion`, `isUnchanged`, `tryLockAt`, `tryLockExclusive`, `lockExclusive`, `unlockExclusive`, `lockShared`, `unlockShared`) |
| `velt/sync/epoch` | epoch-based reclamation (`pin`, `unpin`, `retire`, `collect`) |
| `velt/sync` `Atom<T>` | a `shared` value read without a lock and replaced whole |
| `velt/collections` `ConcurrentMap` | a B+ tree with optimistic lock coupling, `u64` or `string` keys |
| `velt/simd` `loadOptimistic` | a vector load of atomic words, for reads a latch validates |

### Reclamation

`velt/sync/epoch` (`std/sync/epoch/reclaim.velt`) is epoch-based reclamation:

- **State:** a global epoch, a static `Atomic<u64>` moving in steps of 2. Per thread, a slot: an
  `Atomic<u64>` state (the epoch it saw, bit 0 set while in a section) and a
  `shared<Mutex<Garbage>>` of retired values with their epochs, made at the thread's first
  `retire`. Slots live in chunks of 64, linked from a static `Atomic<shared<Chunk> | null>` and
  freed only at exit; their states are 16 words apart, a 128-byte line each. A thread finds its
  slot through a `ThreadLocal<u64>`.
- **`pin` / `unpin`:** store the global epoch with the bit into this thread's state, then a
  `seqcst` fence; `unpin` stores 0 (release). Nesting is counted in a `ThreadLocal`, and a stray
  `unpin()` at depth 0 panics.
- **`retire(v)`:** a `seqcst` fence, then the current epoch: paired with `pin`'s fence, the epoch
  it reads is at least the one current when `v` was unlinked, so `v` cannot expire while a reader
  that could reach it is still in its section. Every 64 retirements it calls `collect`.
- **`collect`:** loads the epoch, a `seqcst` fence, and moves the epoch on by a compare-and-swap if
  no state is active at an older one. Then it drops, on every thread's list, what was retired at
  least two steps before. Draining every list stands in for a thread-exit hook.

### `Atom<T>`

`Atom<T>` is a struct of an `Atomic<shared<T>>` and a `VersionLatch` for writers. `read(f)`
hands the borrowed value to the callback in the method's own epoch section; `load()` returns it
counted; `store` and `swap` are the `Atomic`'s; `update(f)` takes the latch so updates take turns,
loads, calls `f`, and publishes with a compare-and-swap, running `f` again if a `store` or `swap`
came in between. A throw leaves the value as it was.

### `ConcurrentMap`

`ConcurrentMap` is built only on public std APIs. Its nodes are 128-word blocks of one
`Atomic<u64>[]` arena allocated whole by `create`, addressed by index and never moved, so a reader
holding a stale index still reads a node and finds its version changed. Leaves are unsorted, with
16-bit tags of the keys in the first cache line, compared with `loadOptimistic`; inner nodes keep a
6-key index in the first line, then groups of 8 keys and 8 children. A delete that empties a leaf
unlinks it (and inner nodes left with no other child) from the lowest ancestor that keeps a
child, and pushes them on a free list: a stack of node indices in one `Atomic<u64>` with a change
count in its high half against ABA. A node's version only grows, through being freed and reused
too, so a reader or a writer at a stale version fails its check.

## Compilation

As the SIMD operations do ([rt_abi.md](../contracts/rt_abi.md) "SIMD"), lowering calls one
extern per operation, width and ordering, `velt_atomic_<op>_<bits>_<order>[_<failure>]`
([rt_abi.md](../contracts/rt_abi.md) "Atomics"), with the word's address first.
`velt_vir::atomic::AtomicCall` says what each symbol means, the verifier rejects one that means
nothing, and both backends emit every call inline. No runtime function exists behind them.

- **Sema.** The methods are declared in the prelude, so a call is checked as any method call: the
  orderings are parameters of string literal union types, and defaults are placed at the call.
  Sema then rewrites a call of one of `Atomic`'s methods
  (`crates/velt_sema/src/body/expr/atomic.rs`) into an `Atomic*` intrinsic
  ([hir_encodings.md](../contracts/hir_encodings.md)): the receiver a borrowed place, every
  ordering an integer literal. The copy and share checks are
  `crates/velt_sema/src/atomic_copies.rs` and `crates/velt_sema/src/ownership/boundary.rs`.
- **Lowering** (`crates/velt_vir/src/lower/atomic.rs`) computes the receiver's address where it
  is: a local, a field, an element, or the box of a `shared` value read in place, never the copy a
  frozen read makes of a small value. A float is reinterpreted through a one-field aggregate
  (`Proj::Cast`), so backends see only integer externs; its read-modify-writes are the VIR loop
  `float_rmw`. References and strings are `atomic_ref.rs` and `atomic_str.rs`, which use the
  64-bit externs on the pointer's bits; static atomics are `static_atomic.rs`.

| | LLVM (release) | Cranelift (debug, JIT) |
|---|---|---|
| load / store | `load atomic` / `store atomic` with the ordering | `atomic_load` / `atomic_store` |
| exchange, add … min | `atomicrmw <op>` with the ordering | `atomic_rmw` |
| compareExchange(Weak) | `cmpxchg [weak]` with both orderings | `atomic_cas` |
| fence | `fence <order>` | `fence` |
| spinHint | `llvm.aarch64.isb(15)` / `llvm.x86.sse2.pause` | nothing |

Cranelift's atomics are all sequentially consistent, which is correct for every ordering (only
stronger). Its LL/SC loops on arm64 without LSE compare whole registers, which breaks 8- and
16-bit `umax`/`umin` and 32-bit compare-and-swap of narrowed operands; velt_codegen_cl works
around it (`crates/velt_codegen_cl/src/function/atomic.rs`).

LLVM gets the exact orderings, so release builds on arm64 and x86-64 match Rust's code
instruction for instruction (but for two `nop`s on x86-64, below): on aarch64 with LSE a relaxed
or acquire load is `ldr`/`ldapr`, a release store `stlr`, `add` `ldadd`, a compare-and-swap
`casal`; on x86 loads and stores are `mov`, `add` is `lock xadd`. On arm64 Linux (generic
armv8-a) clang gets `-moutline-atomics`, as Rust's targets do: read-modify-writes call the
`__aarch64_*` helpers, which choose LSE at run time.

### Sections

`crates/velt_vir/src/lower/epoch_sections.rs` places the read sections; users write none. A
function with a borrowed reference load calls `velt/sync/epoch`'s `pin` at its entry and `unpin`
before each return. Poll functions and generator resumes keep their dispatch in the entry block,
and a borrow must not outlive a suspension, so there each `load` is a section of its own: `pin`,
the load, the count, `unpin`. A callee with loads of its own costs a depth count, not a fence.
`velt/sync/epoch`'s own functions are exempt: its registry words are only ever set once, so its
borrows need no section. veltc's `epoch_fences` test checks the optimized LLVM IR for the fences
(one `seqcst` fence in a program that only pins, two in one that only retires), since a missing
fence shows only on a weakly ordered CPU and no run can be counted on to hit it.

### Frozen fields

During a call, nothing writes a borrowed value but its atomics' words. A function whose borrowed
parameter holds an `Atomic` therefore gets no `readonly` attribute (its atomic writes go through
that pointer) but gets `frozen` (`crates/velt_vir/src/vir.rs` invariant 9,
`crates/velt_vir/src/lower/param_attrs.rs`): nothing changes the parameter's other bytes during
the call. `frozen` is never set for a type whose values module state can reach, since any callee
could write the argument through a module variable or `static` field the call site cannot see.
An atomic operation never reads or writes anything else, and no byte of an `Atomic` itself is
promoted.

`velt_opt`'s `frozen` pass (`crates/velt_opt/src/frozen.rs`) uses that to load the other scalar
fields a function reads in a loop once, on entry: a node's array headers, its bounds, a pool's
data pointer. The loop keeps them in registers across acquires, fences and calls, and its bounds
checks become loop-invariant. LLVM cannot find this itself: an acquire or a fence may let it see
any memory another thread wrote unless the memory is private or invariant, and a struct holding an
atomic gets neither `noalias` nor `readonly`. Rust has the same problem: a `&T` whose `T` holds an
`UnsafeCell` loses `noalias`, so Rust reloads the headers after every acquire.

**Two `nop`s after an acquiring read-modify-write on x86-64.** With the headers in registers, a
spin lock's critical section shrinks to the store it protects (`lock cmpxchg; je` followed
directly by the increment), and each lock round trip gets about 6 to 8 cycles slower on Intel
cores although it does less. When the critical section's first uop is allocated within one uop
of the locked instruction's, the uops that depend on the locked load are dispatched before its
value is there and dispatched again. Any two uops in between avoid it (one `nop` or one load is
not enough). So the LLVM backend emits two `nop`s after every acquiring (`acquire`, `acq_rel`,
`seq_cst`) `cmpxchg` and `atomicrmw` on x86-64 (`after_acquiring_rmw` in
`crates/velt_codegen_llvm/src/function/atomic.rs`). A `nop` issues but needs no port; relaxed
read-modify-writes and arm64 are unchanged. Hardware counters on a Coffee Lake i9-8950HK (20 M
uncontended lock round trips, per round trip):

| | reloads | frozen | frozen + `nop`s |
|---|---:|---:|---:|
| cycles | 24.5 | 29.3 | 24.6 |
| instructions | 11.0 | 8.0 | 10.0 |
| `cycle_activity.stalls_mem_any` | 11.9 | 18.5 | 14.6 |
| `resource_stalls.any` | 11.7 | 17.7 | 11.9 |

On an Alder Lake i9-12900HK the same probe takes 139.7 ms with reloads, 174.8 ms frozen and
138.4 ms with the `nop`s, while the latch read loops of `bench/atomics` keep the frozen gain
(18.6 ms with reloads, 13.8 ms with the `nop`s). The Apple M4 has no such stall.

## Runtime cost

- **Layout:** an `Atomic<T>` of a number or `bool` is a `T`; a struct of atomics is the struct of
  its `T`s. A reference or string `Atomic` is one pointer.
- **Each operation:** the instruction itself, as in Rust. The box of the `shared<T>` the receiver
  is in is retained only where code could give its variable another handle meanwhile, as for any
  read in place. Programs that use no atomics are unchanged.
- **Against Rust** (`bench/atomics/run.sh`; Apple M4, best of 7; Rust with fat LTO and
  `target-cpu=native`, Velt for the generic target; a threaded workload's time runs from a start
  barrier to the last worker's end):

  | ms | Velt | Rust | Velt / Rust |
  |---|---:|---:|---:|
  | relaxed `add`, 1 thread, 200 M | 322.8 | 321.9 | 1.00 |
  | load + `compareExchangeWeak` loop, 1 thread, 50 M | 82.4 | 82.5 | 1.00 |
  | relaxed `add` on one word, 4 threads × 10 M | 248.8 | 196.1 | 1.27 |
  | relaxed `add`, a word per thread, 4 × 50 M | 89.0 | 89.0 | 1.00 |
  | `VersionLatch` optimistic reads, 4 × 10 M, one writer | 14.3 | 13.6 | 1.05 |
  | the same, no writer | 7.6 | 7.6 | 1.00 |
  | spin lock (`compareExchangeWeak` + `Backoff`), 4 × 2 M | 20.6 | 25.6 | 0.80 |

  The single-threaded loops are the same instructions as Rust's. The contended rows move with
  where the OS puts the threads.
- **Epoch sections:** a `pin`/`unpin` pair costs 2–3 ns on one thread and 24–42 ns with 8 threads
  at once (Apple M4, release). A loop of 10 M loads inside one call costs the same as with no
  sections.
- **`Atom<T>` against Rust's `arc-swap`** (Apple M4, release; 8 threads each reading 10 M times,
  then 4 writers and 4 readers): one `read` per call about the same (65–91 ms against 63–81 ms);
  reads in a loop 42–51 ms against 63–81 ms; mixed 166–187 ms against 546–626 ms.
- **`ConcurrentMap` against bbtree's `BBTreeMap`** (`bench/concurrent_map/run.sh`, 10 M random
  keys, 1–8 threads): on an Intel i9-12900HK it is faster in every row (insert 32–48%, lookup
  24–77%, batched lookup 56–94%, update 13–58%); on an Apple M4 it is faster in every row but
  updates, which trail by 6–9%. A lookup is 344 instructions against bbtree's 349 on arm64.

## Diagnostics

- ``an `Atomic<u64>` cannot be copied or shared: it is used where it is``, with a note to read it
  with `.load()` or pass or keep the object that holds it, for a share of one in a body, a capture
  or a destructuring binding.
- ``an `Atomic<u64>` cannot be copied or shared, but the generic code called here shares one``,
  for generic code instantiated with a type that would get a count (a `Map` of them,
  `xs.slice()`), reported by lowering at the user code that first shared it, never in std.
- ``this copies a part of a `shared` value that holds an `Atomic`: changes to the copy's atomics
  would not reach the shared value``, for `const xs = s.items` and every other deep copy of a
  frozen value holding one.
- ``… `Hits` holds an `Atomic`, and changes to the copy's atomics would not reach this one``, for
  a value holding an `Atomic` outside a `shared` that crosses to another task while the program
  still uses it (a `spawn` argument or capture, a channel send). A value whose last reference goes
  to the task moves there.
- ``` `Atomic<T>` needs a number type, `bool`, `string` or a `shared` value, found `C` ```;
  for a type parameter the note says to bound it with `T extends AtomicValue`.
- `mismatched types` for an ordering the operation cannot have, and
  `an ordering must be a string literal` for one not known at compile time.
- ``` `add` needs an `Atomic` of … ``` for an operation the value type lacks, and
  ``` `compareExchange` is not available on `Atomic<T>`, whose `T` is a type parameter ``` in
  generic code.
- ``` `Atomic`'s method `load` cannot be used as a function value ```.

## Compatibility

The method names and their results are JavaScript's `Atomics`, and the default ordering is its
only one. JavaScript's `Atomics` work on typed arrays over a `SharedArrayBuffer`; Velt has no
`SharedArrayBuffer`, and an `Atomic` is a field or element of any shared value instead.

## Alternatives considered

**Atomics as runtime functions.** A call per operation, always sequentially consistent, costs a
call and the strongest fence where the algorithm needs a plain `mov`; inline externs cost
nothing over the instruction.

**Marking frozen loads `!invariant.load` for LLVM.** The marker promises the memory never changes
anywhere in the program, but Velt objects are written before they are frozen; with a write
through one pointer and an invariant read through another, LLVM folds `p[0] - old` to 0. The
`frozen` pass relies only on the call's own guarantee, so it is sound under any inlining.

**Hazard pointers in the runtime for reclamation.** Epochs in Velt cost one store and fence per
section, need no runtime code, and the compiler places the sections, so users write no guards.

## Limitations

- Epoch sections stay open across a loop's back edges, so a long synchronous loop holding a
  borrowed reference delays drops (memory only).
- `Atomic<string | null>` is not supported (`Atomic<shared<string> | null>` is), and a static
  `Atomic<string>` lives in the module state.
- `ConcurrentMap` has a fixed capacity set by `create`, leaves are never merged and the root never
  shrinks.
- There is no public blocking `wait`/`notify` on an `Atomic`: blocking a worker thread stalls its
  tasks. std reaches the OS's wait and wake calls internally through `atomicAddress`
  ([intrinsics.md](intrinsics.md)).

## Not yet implemented

- **Ordered walk of `ConcurrentMap`** ([#1688](https://github.com/velt-lang/velt/issues/1688)):
  `range`, iteration and `size`. Leaves are unsorted and unlinked, so a walk needs its own
  protocol and a stated consistency.
- **Async-aware waiting** ([#709](https://github.com/velt-lang/velt/issues/709)): a wait on an
  atomic that parks the task, not the worker thread, so mutexes, semaphores and channels can be
  built on atomics in user code.
