# Design: JavaScript semantics without a garbage collector

Status: decided; staged. Stage 1 (strings) and hybrid promises are implemented; stages 2 and 3
are planned.

Goal: TypeScript developers never write `.clone()` or see "use of moved value" in normal code,
while Velt keeps Rust-level performance and memory behavior.

## Hard requirements

1. **No garbage collector.** No tracing collector, no background heap scanning, no heap-size
   tuning.
2. **No pauses.** Memory is freed deterministically at the last use (a reference count reaching
   zero, or the end of unique ownership). Nothing ever stops the program to reclaim memory.
3. **No performance loss.** Every stage is gated by the benchmark suite (see "Gates"). A stage
   that regresses is not merged.

## Model: JS semantics by default, Rust costs only where needed

| Kind | Semantics (user-visible) | Implementation |
|---|---|---|
| numbers, bool, Copy structs | values (JS primitives) | registers and stack, unchanged |
| `string` | immutable value; copy freely, never `.clone()` | small strings inline (≤ 23 bytes, no heap); longer ones in a refcounted immutable buffer; a copy is a refcount increment, elided by the compiler when the source is dead (a move) |
| objects (class instances), arrays, `Map`, closures | **shared references like JS** (`const b = a; b.push(1)` changes `a`) | ownership inference first: a value with a provably single owner uses today's code (no refcount, `noalias`); only values that are actually aliased get a refcount |
| values crossing threads (`spawn`, handlers) | only thread-safe values (`shared`, `Mutex`, immutable data): a compile-time check | atomic counts only for values that cross threads; everything else non-atomic |
| `x.clone()` | explicit independent deep copy (like `structuredClone`) | deep copy |

What disappears: `.clone()` for strings, getters returning fields, `for...of` elements,
`Map.get` and struct literals built from borrowed data; "use of moved value"; the
escaping-closure capture error (`let count = 0; const inc = () => count++` works; a captured
variable that is modified gets a counted box only when the closure escapes).

What stays: no GC, no pauses, deterministic `[Symbol.dispose]()` (it runs when the last
reference goes away), inline values, `noalias` and exclusivity where uniqueness is proven,
compile-time thread safety.

## Reference cycles — without a collector

- **Static cycle analysis**: a type can only leak through a cycle if its type graph can reach
  itself through strong references (`class Node { next: Node | null }`, parent ↔ child). The
  compiler knows exactly which types can form cycles; most can't.
- For cycle-capable types: `weak` references (`parent: weak Node | null`, like Swift and
  Objective-C) and a compiler **warning** on a strong self-reachable field without `weak`,
  explaining the leak risk.
- **No cycle collector by default.** If one is ever added, it must be opt-in per type,
  incremental and bounded (no pause), and off in benchmarks.

## Why performance holds

- Hot code (benchmarks, inner loops) works on uniquely owned data, so it compiles exactly as
  today.
- Refcount operations appear only where JS semantics require sharing. Perceus (Koka) and Lobster
  show that 90–99% of refcount operations can be removed at compile time with ownership and
  borrow inference, which Velt already performs.
- Counts are non-atomic except for thread-crossing values; there is no refcounting on numbers or
  structs; small strings never touch the heap.
- Reuse analysis (Perceus-style): a unique value being dropped can donate its memory to a new
  value of the same size (for example `arr.map(...)` in place).

## Stages (each fully gated)

1. **Strings** (implemented): small-string optimization plus refcounted immutable buffers; no
   string ever needs `.clone()`. The representation is in
   [rt_abi.md "Strings"](../contracts/rt_abi.md): inline (≤ 23 bytes), static, or heap with an
   **atomic** count. One scheme suffices because the count only moves when a string is copied
   while its source stays alive, and unique drops skip the atomic operation, so the measured
   cost stays inside the gate. Sema makes every string move "soft" (a move at the last use, a
   copy otherwise; closures capture by copy), and `s.clone()` compiles as a plain copy. JS
   division (below) shipped in the same stage. Gate results:
   [bench/RESULTS.md "Semantics stage 1"](../../../bench/RESULTS.md).
2. **Objects, arrays, maps, closures** (implemented, [semantics-stage2.md](semantics-stage2.md)):
   shared-reference semantics with uniqueness inference and a refcount fallback; `.clone()` is a
   deep copy; move errors are removed; escaping closures box the captures they modify. Lowering
   counts exactly the types a program shares; everything else keeps the unique-owner code. `==`
   on objects is identity and `deepEqual` compares contents. Not yet: removing the `struct`
   keyword (structs already behave as objects).
3. **Cycles** (planned): `weak`, static cycle-capability analysis and the warning.

## Gates

Per stage, on every primary platform:

- All end-to-end tests (debug and release) pass; semantic changes update the expected outputs
  deliberately and list them.
- `bench/run.{ps1,sh}` (LLVM release): every benchmark's median over at least 10 runs is within
  **±3%** of the pre-stage baseline measured on the same machine in the same session (or
  faster). Any benchmark slower than that blocks the merge.
- HTTP benchmark: requests per second and peak RSS are not worse than the baseline beyond noise.
- Refcount counters (`VELT_RC_STATS=1` with a debug runtime): the number of refcount operations
  per benchmark, before and after; hot loops must show zero.
- No memory growth in a 10-minute HTTP soak test (leak check).

## Numbers

Decided and implemented with stage 1: `/` yields `f64` unless both operands have declared
integer types, so `const a = 7; a / 2` is `3.5` like JS. Integer speed is kept where it matters
(loop counters, array indexes, values declared `i64`, `u8`, …); integer division is explicit
(`Math.trunc(a / b)` on integers lowers to one instruction). The rules are in
[the Reference](../../reference/types.md#numbers).

## Promises

Decided and implemented: **hybrid promises**, with JS-identical results.

| Code | Behavior | Cost |
|---|---|---|
| `await f()` | runs inline | zero: no task, no allocation |
| `const a = f()` … `await a` | starts immediately, like JS | one small local task |
| `f();` (promise dropped) | **compile error** "floating promise", with fixes `await f()` or `spawn(f())` (errors would otherwise be lost) | — |
| `Promise.all([f(), g()])` | concurrent | one allocation per promise |

Eagerly started promises run as **local tasks on the current worker thread** (JavaScript's
single-threaded concurrency model), so they need no thread-safety rules. Only `spawn(...)` moves
work to another core. The concurrency APIs match JavaScript: stored promises, `Promise.all`,
`Promise.race`, `Promise.allSettled`, `Promise.any`; `spawn` adds multi-core parallelism.

How it is built ([rt_abi_async.md §1.1](../contracts/rt_abi_async.md)):

- **Runtime**: a stored promise is a boxed state machine; the call site starts it, which runs it
  to its first suspension and registers it in the **local set** of the task being polled. Every
  task root (`block_on`, spawned tasks, HTTP handlers) polls its woken local promises before its
  own state machine, one at a time: never concurrently with the task, which is JavaScript's
  model per task, and safe on the multi-threaded runtime because a set only moves with its task.
  When a local promise finishes, its awaiter resumes immediately, like a microtask, so output
  orderings match Node. `await f()` (embedded state) and `spawn(f())` (its own task) are
  unchanged, and a task that never stores a promise pays two thread-local writes per poll.
- **Dropping an unawaited promise** follows JavaScript: it runs to completion and its result is
  dropped. If its task's root finishes first, the set moves to an orphan task that keeps the
  process alive until it is done (like Node waiting for pending work). Only a cancelled task
  (for example an HTTP request whose client left) cancels its unfinished local promises.
- **Errors** (typed, see [TypeScript alignment §3](ts-alignment.md)): a started promise keeps its
  result or error in its slot, so `await p` rethrows; `Promise.race` settles with the first to
  settle (rejections included), `Promise.any` skips rejections, `Promise.allSettled` reports
  `reason: E`. A rejection nobody can observe (the promise was dropped unawaited) is reported
  when it happens, like an unhandled rejection (`Uncaught X: msg`, exit code 1).

Planned: **borrowing arguments.** Today every promise owns its inputs (async arguments are
moved, or copied when used again). The planned relaxation lets an argument be passed by
reference when the promise is (a) awaited directly, or (b) stored in a `const` local of the same
function that is awaited on every path before any borrowed value is modified, moved or goes out
of scope, and never escapes (not returned, stored, passed to `spawn` or `Promise.*`, or
captured). Such a promise would be cancelled at scope end on an early exit (`return` or `throw`
before its `await`) instead of running to completion, the one observable difference, so sema
must also prove it is awaited before every exit.

`new Promise((resolve, reject) => …)` compiles to the prelude's `promiseNew`: a settle-once
slot in `shared<Mutex<…>>` plus a runtime latch. `resolve`/`reject` are heap closures over it,
and sema lets a literal executor keep them (`FnInfo::keeps_fn_params`). A guard both hold
marks a promise abandoned unsettled: it never settles, and a direct `await` of it is reported
with the `new Promise` site (`Intrinsic::SourceLocation`).

## JS fidelity decisions

Principle: adopt the best of TypeScript and JavaScript, never their bug sources; prefer one way
of doing things, even when ported code must change (with a precise error and a fix).

- **Objects are references; `==` is identity** (planned, with stage 2). Anonymous objects and
  structs get JS reference semantics (`const q = p; q.x = 2` changes `p`). The compiler keeps
  inline or value storage where the difference can't be observed (immutable objects, uniquely
  owned array elements). The `struct` keyword is removed. `==`/`===` on objects compares
  identity; content comparison goes through `.equals(other)` or a std `deepEqual`.
- **No implicit string coercion** (implemented). `string + string` concatenates;
  `string + number`, `string + bool` and the like are compile errors with the fix "use a
  template literal". This rules out `"5" + 1 === "51"` and `"Total: " + a + b` bugs. Template
  literals are the one way to build text.
- **TypeScript resource management** (implemented). `[Symbol.dispose]()` is the cleanup method
  (run automatically when the value is dropped); `using x = …` disposes at the end of the
  enclosing block; `await using` and `[Symbol.asyncDispose]()` work too.
- **Modules** (implemented). `import * as ns`, `export { x } from`, `export * from`, folders
  through `index.vlt`, `paths` aliases in `package.vlt`, `import type`. Named exports only (no
  `export default`).
- **One "nothing": `null` only** (implemented). `undefined` is not part of the language: using
  it is a compile error with the fix "use `null`". Code ported from TypeScript changes
  `undefined` to `null` (one way of doing things; the `null`-vs-`undefined` bug class can't
  exist). Optional fields and parameters (`a?: T`) are `T | null`. JSON's "absent vs explicit
  null" is visible only through `JsonValue` (`has(key)` vs `isNull()`).
- **Safe truthiness** (implemented). Conditions and `!`, `||`, `&&` accept `bool` and nullable
  values (`if (!user) return;` is a null check; `a || b` and `a && b` return values on nullable
  objects). Numbers and strings are rejected in conditions and in `||`/`&&`, with fixes
  (`count !== 0`, `name !== ""`, `??` for defaults). This removes JavaScript's `0`/`""`/`NaN`
  falsiness bugs.
