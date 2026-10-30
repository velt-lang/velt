# Design: atomics, the platform for lock-free data structures

Status: **accepted** (owner, 2026-10-26: the model, JS `Atomics` names returning the previous
value, string literal orderings, step 1 first); **step 1 implemented**. Supersedes the
runtime-built `Atom<T>` of #708's design: `Atom<T>` becomes a std type written in Velt on the
primitives below, like `BTreeMap` (#1543 step 2: "the map uses no std-only intrinsic or runtime
declaration, only APIs every program has").

## Problem

Velt has two ways to share mutable state between threads: a lock (`Mutex<T>`, `RwLock<T>`) and
the three atomic methods of `shared<i64>` (`add`, `get`, `set`). Neither can express the
techniques the fastest concurrent data structures use:

- **Optimistic lock coupling** (bbtree's concurrent map, `tree/node/latch.rs`): a 64-bit latch
  word per node, read with an acquire load, checked after the reads with an acquire fence and a
  relaxed load, taken with a compare-and-swap that never waits, and let go with a release store
  that bumps the version. A reader takes no lock and writes no shared cache line.
- **Seqlocks, counters, flags, ticket and MCS locks, work-stealing deques, hazard pointers,
  epochs**: all a few words that are loaded, stored, swapped and compared-and-swapped with the
  weakest ordering that is correct.

Before step 1, `shared<i64>`'s methods were calls into the runtime (`velt_rt_atomic_add_i64`),
always sequentially consistent, and there is exactly one word per `shared`.
bbtree's latch needs a word in every node, in place, with acquire and release orderings that are
plain `mov` on x86 and cost nothing on Apple silicon (`ldapr`/`stlr`).

#1543 step 5 says a lock-free map "can't be written in safe Velt". This design makes it
writable, in std or in any package, at the speed of Rust's `core::sync::atomic`.

## Model

**`Atomic<T>` is a word in place.** It is a prelude struct, `struct Atomic<T> { private v: T }`,
for `T` one of `i8 … i64`, `u8 … u64`, `isize`, `usize`, `bool`. It is laid out as a plain `T`
(no count, no box) wherever it is stored: a local, a field of a class or struct, an element of
an array. Its methods are the only way to read or write it, and each compiles to one
instruction (or a short LL/SC or CAS loop where the CPU has no single instruction).

**It is the one part of a `shared<T>` that changes.** A `shared<T>` never changes (#36), and a
frozen read already reaches its fields and elements in place, in its box. An `Atomic` reached
that way is changed in place: `s.nodes[i].version.store(v + 1, "release")` writes the word in
the box. That is sound because an `Atomic` holds no count and no pointer: it is a number every
thread may read and write with atomic instructions, exactly what `shared<i64>.add` does today
for the whole box.

**An `Atomic` is never shared or copied** (sema error). Copying the word would split one atomic
into two values, and sharing one would make lowering count the type (semantics-stage2.md §3),
which boxes it and breaks the "word in place" layout for every program. So:

```text
const v = node.version;            // error: an `Atomic<u64>` cannot be copied or shared;
                                   //   read it with `node.version.load()`
f(node.version);                   // same error, unless `f`'s parameter is borrowed and only
                                   //   calls its methods
return node.version;               // same error
```

An `Atomic` is moved into its place when that place is made (`new Atomic<u64>(0)` in a field
initializer, a struct literal or `Array.from`), like a `Mutex` into `shared(...)`.

**Arrays of atomics are the type-stable memory.** An array inside a `shared<T>` never grows,
shrinks or moves while the box lives. So `shared<Pool>` with `Atomic<u64>[]` fields is a block
of words that every thread may read and write in place, which no thread can free under another:
the property bbtree gets from type-stable pages. A structure that grows publishes new chunks
through a `Once<T>` slot (step 2) instead of reallocating.

## API

```ts
type LoadOrder = "relaxed" | "acquire" | "seqcst";
type StoreOrder = "relaxed" | "release" | "seqcst";
type Order = "relaxed" | "acquire" | "release" | "acqrel" | "seqcst";

struct Atomic<T> {                          // T: integer or bool
  new Atomic<T>(v: T)
  load(order: LoadOrder = "seqcst"): T
  store(v: T, order: StoreOrder = "seqcst"): void
  exchange(v: T, order: Order = "seqcst"): T                  // the previous value
  compareExchange(expected: T, v: T, order: Order = "seqcst",
                  failure: LoadOrder = <from order>): T       // the previous value
  compareExchangeWeak(expected: T, v: T, order?, failure?): bool  // may fail spuriously
  add(v: T, order?): T   sub(…): T   and(…): T   or(…): T   xor(…): T   // previous value
  max(v: T, order?): T   min(v: T, order?): T                          // integers only
  static fence(order: Order): void
  static spinHint(): void                    // `pause` / `isb`, for spin loops
}
```

- **Names follow JS `Atomics`** (`load`, `store`, `exchange`, `compareExchange`, `add`, `sub`,
  `and`, `or`, `xor`), and so do the results: the read-modify-write methods return the
  *previous* value, as `Atomics.add` does and as Rust's `fetch_add` does. A TypeScript reader
  knows them; `compareExchangeWeak`, `max`, `min`, orderings, `fence` and `spinHint` are the
  Velt additions, as in Rust.
- **Orderings are string literal types**, default `"seqcst"` (JS `Atomics`' only ordering).
  An ordering must be a literal (a non-literal is an error: the instruction is chosen at compile
  time), and an ordering the operation cannot have is a type error (`load("release")`), as in
  C++ and Rust. A `failure` ordering defaults from `order` as Rust's `compare_exchange` would
  pick it (`acqrel` → `acquire`, `release` → `relaxed`).
- **`shared<i64>`'s `add`/`get`/`set` stay** and compile through the same inline path, so they
  stop being calls (they keep `add` returning the *new* value; they are a different API).

Written on these in Velt (`velt/sync`): `Backoff` (spins with `spinHint`, twice as long each
round, then yields the thread with `velt_rt_thread_yield`) and `VersionLatch`, bbtree's latch
word (`readVersion`, `tryReadVersion`, `isUnchanged`, `tryLockAt`, `tryLockExclusive`,
`lockExclusive`, `unlockExclusive`, `lockShared`, `unlockShared`), as the reference user of the
primitives.

## How it compiles

As the SIMD operations do (rt_abi.md "SIMD"): lowering calls one extern per operation, width
and ordering, `velt_atomic_<op>_<bits>_<order>[_<failure>]` (rt_abi.md "Atomics"), with the
word's address first; `velt_vir::atomic::AtomicCall` says what each symbol means, the verifier
rejects one that means nothing, and both backends emit every call inline. **No runtime function
exists behind them.**

The methods are declared in the prelude (`std/prelude/sync.vlt`), so a call is checked as any
method call: the orderings are parameters of string literal union types, which makes
`load("release")` a type error, and defaults are placed at the call. Sema then rewrites a call
that resolves to one of `Atomic`'s methods (`body/expr/atomic.rs`) into an `Atomic*` intrinsic
(hir_encodings.md "`Atomic<T>`"): the receiver a borrowed place, every ordering an integer
literal. Lowering (`lower/atomic.rs`) computes the receiver's address where it is: a local, a
field, an element, or the box of a `shared` value read in place (`shared_value`; never the
copy a frozen read makes of a small value), and calls the extern.

| | LLVM (release) | Cranelift (debug, JIT) |
|---|---|---|
| load / store | `load atomic` / `store atomic` with the ordering | `atomic_load` / `atomic_store` |
| exchange, add … min | `atomicrmw <op>` with the ordering | `atomic_rmw` |
| compareExchange(Weak) | `cmpxchg [weak]` with both orderings | `atomic_cas` |
| fence | `fence <order>` | `fence` |
| spinHint | `llvm.aarch64.isb(15)` / `llvm.x86.sse2.pause` | nothing |

Cranelift's atomics are all sequentially consistent, which is correct for every ordering (only
stronger), and fine for debug builds. LLVM gets the exact orderings, so on Apple arm64 and
x86-64 release builds match Rust's code instruction for instruction (but for two `nop`s after an
acquiring read-modify-write on x86-64, see "Frozen fields"): on aarch64 with LSE (every
Apple target) a relaxed or acquire load is `ldr`/`ldapr`, a release store `stlr`, `add` `ldadd`,
a compare-and-swap `casal`; on x86 loads and stores are `mov`, `add` is `lock xadd`. On arm64
Linux (generic armv8-a) clang gets `-moutline-atomics`, as Rust's targets do: read-modify-writes
call the `__aarch64_*` helpers, which choose LSE at run time (`max`/`min` stay LL/SC loops, as
in Rust).

A function whose borrowed parameter holds an `Atomic` in its own memory gets no `readonly`
attribute (vir.rs invariant 9, `param_attrs.rs`): its atomic writes go through that pointer.
It gets `frozen` instead: nothing changes the parameter's other bytes during the call. Velt's
own optimizer (`velt_opt`'s `frozen` pass) uses that to read those fields once, on entry,
where LLVM would reload them after every acquire, fence or call (see "Frozen fields" below).

## What it costs

- Layout: an `Atomic<T>` is a `T`. A struct of atomics is the struct of its `T`s.
- Each operation: the instruction itself, same as Rust. The box of a `shared<T>` the receiver is
  in is retained only where code could give its variable another handle meanwhile
  (`BoxHold::IfReplaceable`), as for any read in place.
- No change for programs that use no atomics. `shared<i64>`'s methods get faster (a call less).

## Diagnostics

- ``an `Atomic<u64>` cannot be copied or shared: it is used where it is`` (note: read it with
  `.load()`, or pass or keep the object that holds it; for `shared(a)` of one still used:
  create it inside the `shared`), for a share of one in a body, a capture or a binding of a
  destructuring declaration (`const { hits } = c`; sema `atomic_copies.rs`).
- ``an `Atomic<u64>` cannot be copied or shared, but the generic code called here shares one``,
  for generic code instantiated with one whose type would get a count (a `Map` of them,
  `xs.slice()`): lowering (`boxing` `counted_atomic_diagnostics`). It is reported at the user
  code that first shared it: each function instance remembers the user expression that first
  requested it (`Cx::fn_sites`), lowering tracks the innermost user expression it is in
  (`Cx::user_site`), and each shared type remembers the first such site, a part the shared
  value it came from (`share_sites`, `share_from`). Never at the standard library's code.
- ``this copies a part of a `shared` value that holds an `Atomic`: changes to the copy's atomics
  would not reach the shared value``, for `const xs = s.items` and every other deep copy of a
  frozen value holding one (sema for the direct form, lowering for the rest, at the user's
  call when the copy is made in the standard library: `s.items.slice()`, `find`, `filter`).
- ``` `c` is still used after `spawn`, so the task would get a copy, but `Hits` holds an
  `Atomic`, and changes to the copy's atomics would not reach this one ``` (note: pass the last
  reference, read the values it needs, or share it: `shared(new Hits(…))`), for a value holding
  an `Atomic` outside a `shared` that crosses to another task while the program still uses it
  (a `spawn` argument or capture, a channel send; sema `ownership/boundary.rs`, which reports
  resources without `clone()` the same way). A value whose last reference goes to the task
  moves there, and an awaited call on the same task changes it in place.
- A module-level `const` whose type holds an `Atomic` is kept in the module state (made once),
  as a non-constant initializer is: evaluated at each use, every use would change a new atomic.
  Its methods act on the word in the state's slot. Reaching a slot takes the module lock
  (rt_abi.md), so tasks using it would run one at a time. A *static atomic*, `const X = new
  Atomic<T>(literal)`, is therefore not kept there: its word is writable data in the program,
  starting as the literal (`vir::StaticData::mutable`), and its methods act on it directly,
  with no lock, from every thread, also in std (whose other module constants are not kept in
  the module state). Every version in a `velt dev` session uses the session's one word
  per constant, so a swap keeps its value.
- ``` `Atomic<T>` needs a number type, `bool`, `string` or a `shared` value, found `string | null` ```
  (a type parameter too: the instruction depends on the type).
- `mismatched types` for an ordering the operation cannot have (``expected "relaxed" | "acquire"
  | "seqcst", found "release"``), and `an ordering must be a string literal` for a value whose
  ordering is not known at compile time.
- ``` `add` needs an `Atomic` of a number ``` (also `sub`, `max`, `min` on `Atomic<bool>`), and
  ``` `or` needs an `Atomic` of an integer or `bool` ``` (also `and`, `xor` on a float).
- ``` `Atomic`'s method `load` cannot be used as a function value ```.

**Floats.** `Atomic<f64>` and `Atomic<f32>` are the words of their bits (`u64`, `u32`):
lowering reinterprets the value through a one-field aggregate (`Proj::Cast`, as float hashing
does), so backends see only integer externs. `load`, `store`, `exchange` and the
compare-and-swaps are the word's. `add`, `sub`, `max` and `min` are a strong compare-and-swap
loop in VIR (`lower/atomic.rs` `float_rmw`): a relaxed load, the operation on the float, and a
swap with the call's ordering whose failure is relaxed and feeds the next try, as Rust's
`fetch_update` and the atomic float crates do. The loop compares words, so a NaN value is no
reason to spin. `max` and `min` are the prelude's `max2` and `min2` (`Math.max`, `Math.min`).
`and`, `or` and `xor` on a float are errors.

## Steps

1. **`Atomic<T>`** (implemented): the type, the sema rules, the inline code in both backends,
   `shared<int>`'s atomics through the same path (their runtime functions are gone),
   `velt/sync`'s `Backoff` and `VersionLatch`, docs (docs/std/sync.md, reference async.md),
   goldens (`tests/golden/std/atomic*.vlt`, `sync_latch.vlt`: every operation at every width,
   the errors, a counter, a spin lock and latch-checked reads on worker threads), and
   `bench/atomics` against Rust (results below).
2. **`Once<T>`**: a slot set once with a `shared<T>` (CAS from empty), read by a frozen borrow
   for as long as its holder lives: no reclamation, since it is never replaced. Growing pools
   publish chunks through it. Then a concurrent `BTreeMap` for word keys and values (`u64`
   → `u64`, bbtree's benchmark) in pure Velt: optimistic coupling through inner nodes, leaves
   latched, `Atomic<u64>` slots, the step-2 map's nodes and inline keys. Benchmarked against
   bbtree's `BBTreeMap` (sync) on 1–N threads.
3. **Reclamation, in Velt** (approved on #1696, 2026-10-27; replaces the `atom` branch's runtime
   hazard pointers: nothing of it is in Rust). Three language features, then std code on them:
   - **`ThreadLocal<T>`** (implemented): a module constant with one value per thread, a number
     or `bool`, emitted as the platform's thread-local storage by both backends (OS thread keys
     where Cranelift has none: `velt dev`, arm64 Windows). Contracts: hir_encodings.md,
     rt_abi.md "Thread-local values".
   - **`Atomic<shared<T>>` and `Atomic<string>`**: a word owning one reference to an immutable
     value. `store`/`exchange`/`compareExchange` move references; `load()` is a frozen borrow
     (#36's rules) that counts nothing; a replaced value is retired, not released.
   - **Read sections placed by the compiler**: opened at the first protected `load`, closed
     before each `await`, `yield`, return and at loop back edges; a borrow live at a close is
     counted there (one atomic increment). No new rule for users.
   - **`velt/sync/epoch`**: epoch-based reclamation (one store and fence per section) on
     `Atomic<u64>` and `ThreadLocal`, with per-thread retire lists. Then `Atom<T>` (#708's API)
     as a std struct, and `ConcurrentMap<K, V>` with string keys (inline prefix and tag, keys
     compared 16 bytes at a time in Velt, #1697) and retired values.

## Results of step 1

`bench/atomics/run.sh` (Apple M4, best of 7; Rust built with fat LTO and `target-cpu=native`,
Velt for the generic target). A threaded workload's time runs from a start barrier to the last
worker's end, so how fast tasks start does not count (see the runtime note below).

| ms | Velt `Atomic<T>` | Rust `std::sync::atomic` | Velt / Rust |
|---|---:|---:|---:|
| relaxed `add`, 1 thread, 200M | 322.8 | 321.9 | 1.00 |
| load + `compareExchangeWeak` loop, 1 thread, 50M | 82.4 | 82.5 | 1.00 |
| relaxed `add` on one word, 4 threads × 10M | 248.8 | 196.1 | 1.27 |
| relaxed `add`, a word per thread, 4 × 50M | 89.0 | 89.0 | 1.00 |
| `VersionLatch` optimistic reads, 4 × 10M, one writer | 14.3 | 13.6 | 1.05 |
| the same, no writer | 7.6 | 7.6 | 1.00 |
| spin lock (`compareExchangeWeak` + `Backoff`), 4 × 2M | 20.6 | 25.6 | 0.80 |

The single-threaded loops are the same instructions as Rust's (`ldadd`, `ldapr`, `stlr`, `casal`,
`dmb ishld`). The contended rows move with where the OS puts the threads.

Found on the way, for follow-ups:

- **Reading a `shared` value in a loop reloads the box's value pointer.** `s.latch` and
  `s.w[i]` of a `s: shared<Words>` load the object pointer out of the box on every iteration,
  since LLVM cannot hoist a load across an acquire without knowing the box never changes:
  1.35× on the latch read loop. Passing the object to a function (`readLoop(s, n)` with
  `readLoop(s: Words, …)`) hoists it, which the benchmark does, and the `frozen` pass (above)
  then keeps the object's fields in registers too. A `shared` value read in place in a loop
  could get the same treatment (its contents are frozen for as long as the box lives).
- **Runtime, macOS:** spawned tasks started about 1.5 ms apart, and with the default worker
  count busy workers seemed to begin on efficiency cores (the same read loop took 16.8 ms on 4
  of 10 workers, 6.6 ms with `VELT_THREADS=4`). Step 1d below.
- **Cranelift 0.132, arm64 without LSE:** its LL/SC loops compare whole registers, which breaks
  8/16-bit `umax`/`umin` and 32-bit compare-and-swap of operands narrowed from wider values;
  velt_codegen_cl works around it (`function/atomic.rs`).

## Frozen fields

What Velt knows and Rust cannot say: during a call, nothing writes a borrowed value but its
atomics' words (vir.rs invariant 9's `frozen`, set where `readonly` would be, and also for a
value holding an `Atomic`, but never for a type whose values module state can reach: any callee
could write the argument through a module variable or `static` field that holds it, which the
call site cannot see; param_attrs.rs "Module state"). An atomic operation never reads or writes
anything else. An atomic's word itself changes, and is read as plain memory where a value is
inspected (`console.log` of an `Atomic`), so no byte of an `Atomic` aggregate is promoted. So
`velt_opt`'s `frozen` pass loads the other scalar fields a function reads in a loop once, on
entry: a node's array headers, its bounds, a pool's data pointer. The loop then keeps them in registers across acquires, fences and calls, and its
bounds checks become loop-invariant, which LLVM moves out.

LLVM cannot find this itself: an acquire or a fence may let it see any memory another thread
wrote, unless the memory is private or invariant, and a struct holding an atomic gets neither
`noalias` nor `readonly`. Rust has the same problem: a `&T` whose `T` holds an `UnsafeCell` (every
atomic) loses `noalias`, so Rust reloads the headers after every acquire.

Marking these loads `!invariant.load` for LLVM was considered and rejected. The marker promises
the memory never changes anywhere in the program, but Velt objects are written before they are
frozen. With a write through one pointer and an invariant read through another, LLVM folds
`p[0] - old` to 0 (tested with clang 21). The pass needs no such promise: it relies only on the
call's own guarantee, so it is sound under any inlining.

**A stall it uncovered on Intel, and the two `nop`s against it.** With the headers in
registers, a spin lock's critical section shrank to the store it protects: bench/atomics'
`spinlock_4t` became `lock cmpxchg; je` followed directly by `incq (%r14)` and the unlocking
`movl`. That made each lock round trip slower on Intel cores although it does less: about 6 to 8
cycles more per acquisition. Hardware counters on a Coffee Lake i9-8950HK (bare Linux, `lock1`
probe: 20M uncontended round trips, per round trip):

| | base (reloads) | frozen | frozen + `nop`s |
|---|---:|---:|---:|
| cycles | 24.5 | 29.3 | 24.6 |
| instructions | 11.0 | 8.0 | 10.0 |
| `cycle_activity.stalls_mem_any` | 11.9 | 18.5 | 14.6 |
| `resource_stalls.any` | 11.7 | 17.7 | 11.9 |
| uops dispatched on ports 0-7 minus `uops_executed.thread` | 5.0 | 8.0 | 4.6 |

Machine clears, store-forwarding blocks, 4K aliasing, L1 misses, the loop stream detector and
the decoders (all from the uop cache) are the same in the three, at zero or near it. The extra
cost is uops dispatched again: when the critical section's first uop is allocated within one
uop of the locked instruction's, the uops that depend on the locked load (the branch on its
flags, the increment's store data) are dispatched before its value is there and dispatched again,
and the lock's load waits longer. Any two uops in between avoid it (measured with hand-written
loops: one `nop` or one load is not enough, two `nop`s, two loads, or two ALU instructions are),
which is why a reload of the header, the base's code, hid it. So the LLVM backend puts two `nop`s
after every acquiring (`acquire`, `acq_rel`, `seq_cst`) `cmpxchg` and `atomicrmw` on x86-64
(`function/atomic.rs`, `after_acquiring_rmw`), between the locked instruction and its branch.
A `nop` issues but needs no port; relaxed read-modify-writes (counters) and arm64 are unchanged.
On the Apple M4 there is no such stall (`lock1` 23.8 ms frozen, 24.2 ms with a load added).

Alder Lake i9-12900HK (WSL), medians of interleaved runs, ms:

| | base | frozen | frozen + `nop`s |
|---|---:|---:|---:|
| `lock1` (20M round trips, 30 runs) | 139.7 | 174.8 | 138.4 |
| `spinlock_4t` alone (30 runs) | 61.0 | 81.6 | 61.4 |
| `cas_loop_1t` alone (30 runs) | 469.6 | 466.5 | 474.1 |
| `latch_reads_4t` (bench/atomics, 16 runs) | 18.6 | 13.4 | 13.8 |
| `latch_reads_quiet_4t` (bench/atomics, 16 runs) | 19.4 | 13.1 | 15.8 |

## Step 1d: spawned tasks start at once

The cause was the Velt scheduler's placement, not slow wake-ups: a spawn stays on the spawning
worker, and nothing else wakes for it. The first of the tasks spawned in a loop then computes
without returning, its siblings wait behind it, and only the monitor's rescue of a stalled
worker (2 ticks of 1 ms, half of the waiting tasks each time) moved them: 4 tasks started at 0,
2.3, 3.6 and 4.9 ms. Now a worker that starts the first poll of a spawned task with tasks never
polled waiting behind it says so, the monitor checks it every 50 µs while it does, and once that
poll has lasted 50 µs the waiting tasks never polled go to sleeping workers, one each
(`velt_rt` `task/runtime/velt/monitor.rs`). Shorter polls (a burst of connections, a broadcast's
flushers) move nothing, and the monitor checks that often only while such a poll runs and some
worker sleeps: a program waiting on a 2 s timer, before or after a hundred bursts of 8 short
spawns, used 0.00 s of CPU. The efficiency-core effect above did not reproduce on the
measurement day; asking macOS for the user-interactive quality of service for the workers made
no measurable difference, and Apple reserves that class for UI work, so the workers keep the
default.

A watch is published only while another started worker sleeps (never with `VELT_THREADS=1`).
It hurries the monitor out of its whole-tick nap only until 4 hurried wakes in a row have moved
nothing; a pass that moves tasks allows them again, and until then the next 1 ms tick finds a
long poll anyway. The monitor holds its `quiet::Moving` guard before it takes the tasks from the
queue, as the stall rescue does: between the two the tasks were in no queue and uncounted, and a
worker parking last in that window ended the process with `main` pending (the review reproduced
it with a sleep in the window). On Windows the monitor naps with `thread::sleep` in slices of
50 µs at most, looking for a hurry between them: a condition variable's timeout there rounds up
to the 15 ms timer tick, where `thread::sleep` takes about 0.6 ms for 50 µs and 1.5 ms for 1 ms
(Windows 11, a probe of this `nap`, medians of 400: 1.11 ms for a 1 ms nap and
0.56 ms for a 50 µs one, against 15.4 ms with the condition variable; a hurry from another
thread ends a nap in 49 µs, so the stall rescue takes about 2–3 ms there).

`bench/async/scheduler/spawn_start.vlt` is the start probe below; `spawn_burst.vlt` is 100k
rounds of 2 tiny spawned tasks awaited together, whose first polls are all watched. Apple M4,
a loaded machine (load average 10–30 from another build), medians of interleaved runs:

| `spawn_burst`, workers | wall, before | wall, after | CPU, before | CPU, after |
|---|---:|---:|---:|---:|
| 1 | 115.0 ms | 115.5 ms | 43.0 ms | 43.5 ms |
| 4 | 115.2 ms | 114.5 ms | 42.6 ms | 42.7 ms |
| default (10) | 114.0 ms | 114.1 ms | 42.8 ms | 42.6 ms |

With a hurry at every watch (the first version of this step) the same runs took 129–134 ms and
78–88 ms of CPU, with 2.6–3× the context switches. Starts of the 4 tasks of `spawn_start`
(spread from the first start to the last, 21 interleaved runs):

| workers | spread, before | spread, after | total, before | total, after |
|---|---:|---:|---:|---:|
| default (10) | 6.03 ms | 0.12 ms | 27.6 ms | 22.2 ms |
| 4 | 7.62 ms | 0.11 ms | 28.2 ms | 20.9 ms |
| 2 | 9.22 ms | 5.96 ms | 29.8 ms | 26.7 ms |
| 1 | 9.23 ms | 9.04 ms | 29.9 ms | 29.7 ms |

After 20k tiny bursts (hurries stopped), the first group of 4 started 0.4–1.3 ms apart and the
next 0.10–0.17 ms. Still open: a task that spawns a child and then spins on it without awaiting
waits for the stall rescue (about 2 ms), as before.

A probe spawning 4 tasks of 20M dependent multiplies each (Apple M4, a quiet machine, 5 rounds):
before, starts 0 / 2.2–3.1 / 3.7–4.8 / 5.3–6.3 ms after the first and 39–46 ms in all; after,
the last start 0.11–0.25 ms after the first (sometimes one at 0.5–2 ms) and 31–36 ms in all;
Rust's `thread::scope`, 0.02 ms and 39–42 ms. `bench/atomics` without its start barrier (the
clock runs from before the spawns, on both sides; best of 7):

| ms | Velt before | Velt after | Rust |
|---|---:|---:|---:|
| relaxed `add` on one word, 4 threads | 320.2 | 315.7 | 308.9 |
| relaxed `add`, a word per thread | 96.8 | 92.3 | 92.1 |
| `VersionLatch` reads, one writer | 18.6 | 14.3 | 13.9 |
| the same, no writer | 13.6 | 11.1 | 10.7 |
| spin lock | 9.6 | 27.2 | 25.8 |

(The spin lock was faster before because its threads started one by one and contended less.)
How fast tasks start no longer shows outside the noise; the benchmark keeps its barrier, which
measures the atomics alone. `bench/atomics/run.sh 7` the same day, with the barrier (the machine
ran about 10% slower than for the step 1 table): 354.6 / 352.0 (1.01), 90.1 / 90.1 (1.00),
356.1 / 316.9 (1.12), 92.0 / 91.8 (1.00), 13.4 / 13.8 (0.97), 11.0 / 10.4 (1.06), 25.3 / 24.0
(1.05). The contended rows move with where the OS puts the threads, as before.

## Results of step 2

The concurrent map is `velt/collections` (`ConcurrentMap`, `u64` → `u64`), in
pure Velt on public APIs. It differs from the plan above in two ways. Its nodes are 1 KB blocks
of one `Atomic<u64>[]` arena taken whole by `create`, a fixed capacity instead of growing pools
behind `Once<T>`: nodes never move or are freed, so the arena needs no reclamation. Its leaves
are unsorted, unlike bbtree's sorted ones:
- **Leaves:** 16-bit tags of the keys in the first line. A lookup compares them with velt/simd
  `loadOptimistic` (step 2a) and reads one pair, or no pair for an absent key. A key sits in its
  home slot when that was free, so `add` writes the home pair first and checks after.
- **Inner nodes:** a 6-key index in the first line, then groups of 8 keys and 8 children, one
  line each.
- **Lookup:** the descent runs a fixed number of levels (the height is stored in the root).

The `frozen` pass through field pointers and the `bounds` pass (step 2b) take a lookup from 432
to 344 instructions, against bbtree's 349, with fewer cache misses (cachegrind, 1M keys, arm64
Linux on the M4). On x86-64 the same counts, including each harness's key mixing, are 587
against 568 for a lookup and 549 against 549 for an update (i9-12900HK).

`bench/concurrent_map/run.sh`: 10M random keys inserted, then each thread looks up keys half of
which are present; batched lookups (`getManyInto` against bbtree's `get_many`, 64 at a time); an
update of present keys (`add` against bbtree's `update`); and a mix of 90% lookups and 10%
updates. Apple M4, best of 7, Velt against bbtree:

| workload | 1 thread | 2 | 4 | 8 |
|---|---|---|---|---|
| insert | 19% faster | 20% faster | 23% faster | 19% faster |
| lookup (half present) | 19% faster | 16% faster | 17% faster | 6% faster |
| batched lookup | 40% faster | 37% faster | 40% faster | 30% faster |
| update | 7% slower | 6% slower | 9% slower | 9% slower |
| 90% lookup, 10% update | 12% faster | 11% faster | 10% faster | 3% faster |

On an Intel i9-12900HK (7 interleaved runs, medians, the map's code at #1644's review):

| workload | 1 thread | 2 | 4 | 8 |
|---|---|---|---|---|
| insert | 6% faster | 6% faster | 22% faster | 30% faster |
| lookup (half present) | 19% faster | 12% faster | 14% faster | 7% faster |
| batched lookup | 56% faster | 56% faster | 49% faster | 27% faster |
| update | 16% faster | 7% faster | 3% faster | 1% slower |
| 90% lookup, 10% update | 14% faster | 6% faster | 4% faster | 9% faster |

After the leaf split stopped using `splice` and #1640's last fixes, the same reviewer measured the
map ahead of bbtree in every row on that machine (7 interleaved runs, medians): insert 32–48%,
lookup 24–77%, batched lookup 56–94%, update 13–58% and the mix 27–67% faster, from 1 to 8
threads. Instructions per op on x86-64 then: insert 1,170 against 766, lookup 431 against 568,
update 397 against 549.

On the M4 updates still trailed in the last quiet measurement. A lookup of a present key costs our
map more CPU than bbtree's: with the map in cache, 18% more (the tag compare chain feeds the pair's
load). An update is such a lookup plus a latch. Two Apple-specific findings shaped `add`. First, a
line read shared and then written by a read-modify-write is fetched twice. So `add` takes the leaf
with one `or` and adds to the home pair before reading its key, and that took updates from 42%
slower to the numbers above. Second, putting home-slot checks into lookups made them slower: an
unpredictable branch, and an extra line read for an absent key.

Room comes back when a leaf empties (#1689). The delete of a leaf's last key takes every node
from the lowest ancestor with another child down to the leaf, at the versions read on the way
down. It removes that ancestor's child and a key next to it, so a neighbour covers the empty
range, and pushes the leaf and the inner nodes in between (each left with one child) on a free
list. The list is a stack of node indices in one `Atomic<u64>`, with a count of changes in its
high half against ABA, linked through the free nodes' meta words. A node's version only grows,
through being freed and reused too, so a reader holding a stale index fails its check as with
a split. The bump on free is what keeps writers out of reused nodes: a `set` that stopped at a
leaf after validating its parent takes it with `tryLockAt` at the version it read, and without
the bump would put its key into a leaf of another range (readers would be safe without it, since
a freed node's range was empty). A split's new node may be initialized without its latch: it is
reachable only through its new parent, which is unlocked after, and anyone still holding it
from before the free holds an older version. Leaves are not merged, and the root never shrinks.

What the map does not do yet: an ordered walk (`range`, iteration, `size`; leaves are unsorted
and unlinked). A leaf split sorts its 56 pairs with an insertion sort into two small arrays.

## References (#1696 step 2b)

`Atomic<shared<T>>` and `Atomic<shared<T> | null>`: the word is the pointer of one counted
reference that the `Atomic` owns. It is its field, so the struct's drop glue releases it, and
null is none. Lowering (velt_vir `lower/atomic_ref.rs`) uses the 64-bit `velt_atomic_*` externs
on the pointer's bits, so backends see nothing new.
- **`load`:** a load, uncounted: the reference is *borrowed*. Read in place (`a.load().f`, a
  borrowing argument) or bound by a `const` (sema `borrowed_load` binds it by reference, and
  lowering's `let_pat` binds the loaded pointer without a drop), it costs the load alone. A
  consumer that keeps it counts it, as for any borrowed place (`consume`'s share). The
  function runs in an epoch section ("Sections"), so a borrow stays valid until it returns. In
  an async function or a generator, `load` counts at once instead, inside a section of its
  own, and sema binds such a `const` as a value.
- **`store(v)` and `exchange(v)`:** `v` moves in, by an atomic exchange. `exchange` gives the
  caller a counted copy of the old reference.
- **`compareExchange(expected, v)`:** a compare-and-swap of the pointers, so it compares
  identity. It returns whether it swapped, as `bool` (sema types the call so; the declared
  `T` is for numbers), and `v` is dropped when it did not. `expected` is only read: a place
  argument is not shared first. Its owner keeps it alive, so its address cannot be reused
  meanwhile (no ABA).
- **Retiring:** the reference a store, an exchange or a successful compare-and-swap takes out of
  the word is *retired*, not released. Another thread may have loaded the pointer and not
  counted it yet; releasing it could free the box under that increment. Lowering calls
  `velt/sync/epoch`'s `retire<U>` with it (none for null), which drops it once no thread in a
  section can still read it.

## Reclamation (#1696 step 2d)

`velt/sync/epoch` (std/sync/epoch/index.vlt) is epoch-based reclamation in Velt.
- **State:** a global epoch, a static `Atomic<u64>` moving in steps of 2. Per thread, a slot:
  an `Atomic<u64>` state (the epoch it saw, bit 0 set while in a section) and a
  `shared<Mutex<Garbage>>` of retired values with their epochs, which the slot's thread makes
  at its first `retire` (a thread that only reads makes none). Slots live in chunks of 64,
  linked from a static `Atomic<shared<Chunk> | null>` and never freed; their states are 16
  words apart, a 128-byte line each. A thread finds its slot through `ThreadLocal<u64>`, its
  number + 1.
- **`pin` / `unpin`:** store the global epoch with the bit into this thread's state, then a
  `seqcst` fence; `unpin` stores 0 (release). Nesting is counted in a `ThreadLocal`.
- **`retire<T>(v)`:** a `seqcst` fence, then the current epoch (crossbeam's `push_bag`): paired
  with `pin`'s fence, the epoch it reads is at least the one current when `v` was unlinked, so
  `v` cannot expire while a reader that could reach it is still in its section. It pushes `v` as
  an interface value (`Held<T>` owning the `shared<T>`) with that epoch onto this thread's list.
  Every 64 retirements it calls `collect`.
- **A stray `unpin()`** (at depth 0) panics: wrapping the depth would make every later `pin()`
  on that thread announce nothing.
- **`collect`:** loads the epoch, a `seqcst` fence (pairs with `pin`'s), and moves it on by a
  compare-and-swap if no state is active at an older epoch. Then it drops, on every thread's
  list, what was retired at least two steps before. Draining every list stands in for a
  thread-exit hook, which Velt does not have.
- **Reads:** the registry words are only ever set once (null to a chunk), so the module reads
  them as uncounted borrows without sections of its own.
- **Tests:** `std/epoch` (one thread, nesting), `std/epoch_threads` (a task on another thread
  in a nested section blocks every drop; 8 threads retiring at once drop each value once) and
  `std/epoch_unpin` (the stray `unpin()` error). `std/epoch_threads` catches a section that
  closes early (20 of 20 runs, debug and release) and an advance that ignores active threads.
  A missing fence (in `pin` or `retire`) shows only when a reader on a weakly ordered CPU
  dereferences a value dropped too soon, which no run can be counted on to hit: 2e's threaded
  goldens, freeing for real under the debug runtime's use-after-free checks, did not catch
  either mutant in 25 runs on an M4. So veltc `epoch_fences` checks the optimized LLVM IR: one
  `seqcst` fence in a program that only pins, two in one that only retires. Each mutant fails
  it.

## Sections (#1696 step 2e)

velt_vir `lower/epoch_sections.rs` places the sections. A function with a borrowed reference
load (`FnLower::ref_loads`) calls `velt/sync/epoch`'s `pin` at its entry and `unpin` before each
`Return`: the entry's code moves to a block of its own behind the `pin` call, and jumps to the
entry are redirected there.
- **Suspensions:** poll functions and generator resumes keep their dispatch in the entry block,
  and a borrow must not outlive a suspension. So there each `load` is a section of its own:
  `pin`, the load, the count, `unpin`. Sema binds `const c = a.load()` there by value.
- **Nesting:** `pin` counts depth in a `ThreadLocal`, so a callee with loads of its own costs a
  depth count, not a fence.
- **Exempt:** `velt/sync/epoch`'s own functions (`FnLower::epoch_exempt`, by qualified name).
  Its registry words are only ever set once, so its borrows need no section.
- **Not done:** sections stay open across a loop's back edges, so a long synchronous loop
  delays drops (memory only).
- **Measured** (M4, release, load ~140): a `pin`/`unpin` pair costs 2–3 ns on one thread and
  24–42 ns with 8 threads at once. A loop of 10M loads inside one call costs the same as with
  no sections.
- **Hold rule:** `shared_value` used to keep a counted hold on every narrowed handle
  (`x` after `x == null` returned). That was one contended increment and decrement per `pin`
  on the registry's box; a narrowed handle is now held only if what it narrows is replaceable.
- **Tests:** `std/atomic_ref_sections`. A task on another thread is inside a function holding a
  loaded value while main replaces it 10 times and collects 100 times: nothing is dropped
  until it returns. Without sections, all 10 are dropped and the holder reads a freed string.

## Strings (#1696 step 2f)

A string is three words (rt_abi.md), so an `Atomic<string>`'s word points to a counted box
holding one: the box a `shared<string>` has. velt_vir `Cx::adt_field_tys` lays the
`Atomic<string>`'s field out as `shared<string>`, so its drop glue releases the box, and
`new Atomic<string>(s)` boxes `s` (`adt_lit`). `lower/atomic_str.rs` lowers the methods:
- **`load`:** the string in the box, borrowed, as a reference `load` is (the function's
  section keeps the box). In an async function or a generator it is copied at once, inside a
  section of its own.
- **`store(v)` / `exchange(v)`:** box `v` and swap it in. `exchange` gives a copy of the string
  before. The old box is retired as `shared<string>`.
- **`compareExchange(expected, v)`:** compares contents. It loads the box (acquire), compares
  its string with `expected`, and swaps the pointer if no writer replaced the box meanwhile. A
  strong one reads again when another writer did; a weak one fails then. It returns `bool` and
  drops `v`'s box when it did not swap.
- **Not supported:** a static `Atomic<string>`, whose box is made at run time, stays in the
  module state. `Atomic<string | null>` is not supported yet (`Atomic<shared<string> | null>`
  is).

## `Atom<T>` (#1696 step 3)

`velt/sync`'s `Atom<T>` is plain Velt on the pieces above: a struct of an `Atomic<shared<T>>`
and a `VersionLatch` for writers.
- **Reads:** `read(f)` loads the value borrowed and hands it to the callback, with no count, in
  the method's own epoch section. `load()` returns it counted.
- **Writes:** `store` and `swap` are the `Atomic`'s.
- **`update(f)`:** takes the latch, so updates take turns. It loads, calls `f`, and publishes
  with a compare-and-swap. A `store` or `swap` that came in between makes it run `f` again on
  the newer value. A throw leaves the value as it was, and `finally` lets the latch go.
- **Measured** against Rust's `arc-swap` (M4, release, load ~70; 8 threads each reading 10M
  times, then 4 writers and 4 readers):
  - one `read` per call: 65–91 ms against 63–81 ms, about the same;
  - reads in a loop: 42–51 ms against 63–81 ms;
  - mixed: 166–187 ms against 546–626 ms, about 70% less time.

  Each `read` opens a section; a raw `Atomic<shared<T>>` read in a loop inside one function
  pays it once (6–13 ms).

## Generic `Atomic<T>` (#1696 step 4.1)

`Atomic<T>` with `T` a type parameter is allowed when `T` is bounded by the prelude's marker
interface `AtomicValue` (std/prelude/sync.vlt). The compiler knows its implementations (no
`extend` declares them): `Ctx::satisfies` takes the integer types, `f32`, `f64`, `bool`,
`string`, `shared<U>` and `shared<U> | null`. The bound is checked where the generic is
instantiated, as any interface bound is; lowering instantiates per type already, so `atomic_word` and
`atomic_ref_value` see the substituted `T` and pick the instruction as for written types.
- **Operations:** sema allows on such a `T` only those every `AtomicValue` has alike:
  `load`, `store`, `exchange` and `compareExchangeWeak`. `compareExchange` returns the value
  before for a number but whether it swapped for a reference, and `add` and the bitwise methods
  exist for numbers only.
- **Borrowing:** sema cannot tell a reference `load` from a number's in generic code, so
  `const c = a.load()` there binds by value; lowering counts a reference then (no borrow).
- **Lowering fix:** `atomic_place`'s fallback now substitutes the receiver's type.
- **Why:** `ConcurrentMap<K, V>` (step 4) declares `Atomic<V>` arrays.

## The `Frozen` bound (#1696 step 4.2b)

Sema reads a `shared<T>` as a `T` only when `Ctx::frozen_readable` accepts `T`, and a type
parameter never was. The prelude's marker interface `Frozen` is compiler-known like
`AtomicValue`: `Ctx::satisfies` decides it for a concrete type with `frozen_readable`, and
inside a generic body `FnCx::frozen_readable_here` (`Ctx::frozen_readable_under`) passes the
parameters bounded by it to `frozen_readable_in`, which takes them as readable. Lowering is
unchanged but for one case: at instantiation the type is concrete and the frozen machinery reads
it as for a written type, except that a `shared` 64-bit integer, an atomic one, is read with the
atomic load `get()` uses.
`ConcurrentMap<K, V>` (step 4.3) stores a non-`u64` `V` as `shared<V>` and returns it this way.

## Not in this design

- `Atomics.wait` / `notify` (futex): blocking a worker thread stalls its tasks; an async-aware
  wait needs the scheduler (#709).
- JS `Atomics` on typed arrays: Velt has no `SharedArrayBuffer`.
