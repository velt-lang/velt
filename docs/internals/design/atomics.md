# Design: atomics, the platform for lock-free data structures

Status: **accepted** (owner, 2026-10-26: the model, JS `Atomics` names returning the previous
value, string literal orderings, step 1 first); step 1 is in progress. Supersedes the
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

Written on these in Velt (`velt:sync`): `Backoff` (spins with `spinHint`, twice as long each
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
x86-64 release builds match Rust's code instruction for instruction: on aarch64 with LSE (every
Apple target) a relaxed or acquire load is `ldr`/`ldapr`, a release store `stlr`, `add` `ldadd`,
a compare-and-swap `casal`; on x86 loads and stores are `mov`, `add` is `lock xadd`. On arm64
Linux (generic armv8-a) clang gets `-moutline-atomics`, as Rust's targets do: read-modify-writes
call the `__aarch64_*` helpers, which choose LSE at run time (`max`/`min` stay LL/SC loops, as
in Rust).

A function whose borrowed parameter holds an `Atomic` in its own memory gets no `readonly`
attribute (vir.rs invariant 9, `param_attrs.rs`): its atomic writes go through that pointer.

## What it costs

- Layout: an `Atomic<T>` is a `T`. A struct of atomics is the struct of its `T`s.
- Each operation: the instruction itself, same as Rust. The box of a `shared<T>` the receiver is
  in is retained only where code could give its variable another handle meanwhile
  (`BoxHold::IfReplaceable`), as for any read in place.
- No change for programs that use no atomics. `shared<i64>`'s methods get faster (a call less).

## Diagnostics

- ``an `Atomic<u64>` cannot be copied or shared: it is used where it is`` (note: read it with
  `.load()`, or pass or keep the object that holds it), for a share of one in a body or a
  capture (sema `atomic_copies.rs`); in generic code instantiated with one, lowering reports it
  if the type would get a count.
- ``this copies a part of a `shared` value that holds an `Atomic`: changes to the copy's atomics
  would not reach the shared value``, for `const xs = s.items` and every other deep copy of a
  frozen value holding one (sema for the direct form, lowering for the rest).
- ``` `Atomic<T>` needs an integer type or `bool`, found `string` ``` (a type parameter too: the
  instruction depends on the type).
- `mismatched types` for an ordering the operation cannot have (``expected "relaxed" | "acquire"
  | "seqcst", found "release"``), and `an ordering must be a string literal` for a value whose
  ordering is not known at compile time.
- ``` `add` needs an integer `Atomic` ``` (also `sub`, `max`, `min` on `Atomic<bool>`).
- ``` `Atomic`'s method `load` cannot be used as a function value ```.

## Steps

1. **`Atomic<T>`** (in progress): the type, the sema rules, the inline code in both backends,
   `shared<int>`'s atomics through the same path (their runtime functions are gone),
   `velt:sync`'s `Backoff` and `VersionLatch`, docs (docs/std/sync.md, reference async.md),
   goldens (`tests/golden/std/atomic*.vlt`, `sync_latch.vlt`: every operation at every width,
   the errors, a counter, a spin lock and latch-checked reads on worker threads), and
   `bench/atomics` against Rust.
2. **`Once<T>`**: a slot set once with a `shared<T>` (CAS from empty), read by a frozen borrow
   for as long as its holder lives: no reclamation, since it is never replaced. Growing pools
   publish chunks through it. Then a concurrent `BTreeMap` for word keys and values (`u64`
   → `u64`, bbtree's benchmark) in pure Velt: optimistic coupling through inner nodes, leaves
   latched, `Atomic<u64>` slots, the step-2 map's nodes and inline keys. Benchmarked against
   bbtree's `BBTreeMap` (sync) on 1–N threads.
3. **Reclamation**: an atomic slot of `shared<T>` that can be replaced (`swap`, `compareExchange`)
   with a protected `load()` (the hazard pointers of the `atom` branch's runtime, exposed per
   slot). `Atom<T>` (#708) is then a Velt struct in std, and so are generic keys and values for
   the concurrent map (a replaced key or value is retired, not freed, while a reader may hold
   it).

## Not in this design

- `Atomics.wait` / `notify` (futex): blocking a worker thread stalls its tasks; an async-aware
  wait needs the scheduler (#709).
- JS `Atomics` on typed arrays: Velt has no `SharedArrayBuffer`.
- `Atomic<f64>`: an `Atomic<u64>` holding the bits, once Velt has `f64` bit casts.
