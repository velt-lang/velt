# Design: iteration, generators and `for await`

Status: accepted (issue #62), implemented in four phases. **Phases 1 (the protocol and
`for...of` over user iterables) and 2 (sync generators) are built**; phases 3–4 below are the
plan and are updated as they land.

## Problem

TypeScript code iterates its own data structures (`for (const x of tree)`), streams lines and
messages (`for await (const line of lines)`), and writes lazy sequences with generators
(`function*`). Before this work, Velt's `for...of` only knew arrays, maps and classes with an
`entries()` method that builds an array, and there was no generator or async iteration at all.

## 1. The protocol (built, phase 1)

```ts ignore
type IteratorResult<T> = { done: false; value: T } | { done: true };

interface Iterator<T, E = never> {
  next(): IteratorResult<T> throws E;
  return(): void {}                       // early exit: release what the iterator holds
}
interface Iterable<T, E = never> {
  [Symbol.iterator](): Iterator<T, E>;
}

interface AsyncIterator<T, E = never> {
  next(): Promise<IteratorResult<T>, E>;
  async return(): Promise<void> {}
}
interface AsyncIterable<T, E = never> {
  [Symbol.asyncIterator](): AsyncIterator<T, E>;
}
```

They live in `std/prelude/iter.vlt`.

- The done case has no `value` (Velt has no `undefined`), and there is no `TReturn`.
  `IteratorResult<T>` is an ordinary discriminated union on a `bool` literal field; `if (r.done)`
  and `if (!r.done)` narrow it (sema `body/narrow.rs`: a member access tested for truthiness on
  a union local whose discriminant values are all `bool` literals), and a union of `bool`
  literals is a condition (`body/expr/truthiness.rs`).
- Errors are typed: `E` is what `next()` throws. `for...of` rethrows it.
- Not in the first version: `next(value)` (TS's `TNext`) and `throw()`. A later addition does
  not break code.

### What the protocol needed from the type system

- **Type parameter defaults** (`E = never`). Classes, structs, interfaces and type aliases take
  `T = Default` (parser `parse_type_generic_params`; sema `resolve.rs` `with_defaults` fills the
  missing arguments when a type is named, a default may mention earlier parameters). Functions,
  methods and arrows still reject defaults (TS uses them there only when inference fails).
- **A `throws` clause that names the interface's own type parameters.** Dispatch groups (sema
  `throws/groups.rs`) required one error type without type parameters per interface slot. Now a
  written clause on a generic interface's method may mention the interface's parameters: each
  member (implementation, default body, override) sees it with the interface arguments of its
  own `implements` (`member_bound`, walking `implements`, interface `extends` and base classes),
  and a call through an `Iterator<T, E>` value throws the clause substituted with the value's
  type arguments. This is sound because each instantiation of a generic interface has its own
  vtables: `Iterator<string, IoError>` and `Iterator<string, never>` entries never mix. HIR:
  `InterfaceMethodDef::throws` carries the slot's error type in the interface's parameters
  (contract change, hir_encodings.md "Errors"); VIR substitutes it at `Callee::Dyn` calls.
  An *inferred* group error type, and a base class method's clause, still cannot mention type
  parameters.
- **Inferring `E` from an implementation.** Matching `Iterable<i64, E>` against a class that
  implements `Iterable<i64>` binds `E = never`: interface arguments are matched like error types
  (`infer.rs` `match_iface_args`), where `never` binds a parameter.
- **Symbol method keys**: `[Symbol.iterator]` and `[Symbol.asyncIterator]` are method names like
  `[Symbol.dispose]` (parser `symbol_keys.rs`).

### Deviations from the accepted text

- `Iterator.return()` has an empty default body (`AsyncIterator.return()` an empty `async` one),
  so iterators that hold nothing need not write it; TS declares it optional (`return?()`).
- `[Symbol.iterator]()` of a class implementing `Iterable<T>` must be declared to return
  `Iterator<T, E>` (implementations match the interface signature exactly; Velt has no
  covariant returns). A class that does not declare `implements Iterable` may return its
  concrete iterator class, which `for...of` then calls directly.

## 2. `for...of` over an iterable (built, phase 1)

`for (const x of src)` keeps its array, `Map` and `entries()` loops unchanged. When `src`'s type
is not an array and has a `[Symbol.iterator]()` method (a class or struct, an `Iterable<T, E>`
value, or a bounded type parameter), sema (`body/for_iter.rs`) desugars the statement before
checking it:

```text
{
  let <iterator@N> = src[Symbol.iterator]();        // once; checked as HIR
  let <open@N> = false;
  try {
    label: while (true) {
      <open@N> = false;
      const <result@N> = <iterator@N>.next();       // throws E
      if (<result@N>.done) { break; }
      <open@N> = true;
      const pattern = <result@N>.value;             // `let` for `for (let ...)`
      { body }
    }
  } finally {
    if (<open@N>) { <iterator@N>.return(); }
  }
}
```

- `<open>` is true exactly while the body runs, so `return()` is called once when the body is
  left early (`break`, `return`, a thrown error, a labelled `break`/`continue` of an outer
  statement), and not on normal completion, `continue`, or an error thrown by `next()` itself,
  which is JS's behaviour (an iterator whose `next()` threw is finished).
- The `[Symbol.iterator]()` result must be an `Iterator<T, E>` (an interface value, or a class
  implementing it): else `` `[Symbol.iterator]()` must return an `Iterator<T>`, found `X` ``. An
  iterator is not iterable (TS requires `[Symbol.iterator]()` too); the "cannot iterate" error
  then says the value looks like an iterator.
- Everything after the first statement is checked like source code, so the existing rules apply
  unchanged: narrowing on `done`, typed errors (the `next()` call is an ordinary throwing call,
  so a function containing the loop throws `E`, inferred or checked against its `throws`
  clause), `try`/`finally` lowering, and ownership — the iterator is a local the block owns and
  drops at its end, and each `value` is moved out of the result into the binding.
- The hidden names cannot be written in source (`<`), and are unique per statement.
- A direct generator call (`for (const x of gen(args))`, or a generator `[Symbol.iterator]()`
  method) takes the embedded path of section 3 instead of the protocol.

**Cost.** One `[Symbol.iterator]()` call per loop; per element, one `next()` call (direct for a
concrete iterator class, through the vtable for an `Iterator<T>` value), a tag test on the
result (a small union value, no allocation), and two stores to a local flag. Leaving the loop
runs a `finally` block (no unwinding: errors are result returns). Iterating an
`Iterable<T>` *value* boxes the iterator it returns once per loop (an interface value owns a
heap box). Array and map loops are untouched.

**Diagnostics.**

| Code | Message |
|---|---|
| `for (const x of 5)` | `` cannot iterate over a value of type `i64` ``, noting what `for...of` accepts (arrays, `Map`s / `entries()` classes, `[Symbol.iterator]()`) |
| iterating an iterator | the same, plus "`Counter` looks like an iterator: iterate the iterable that creates it, or give it a `[Symbol.iterator]()` method" |
| `[Symbol.iterator](): Counter[]` | `` `[Symbol.iterator]()` must return an `Iterator<T>`, found `Counter[]` `` |
| `next()` returning `{ done: false, value: 1 }` for `Iterator<string>` | the usual type mismatch at the literal |
| a loop over `Iterable<T, Read>` in a function `throws Other` | `` `f` throws `Read`, which its `throws` clause does not allow `` at the loop |
| `next()` throwing in `implements Iterator<i64>` | `` `C.next` throws `Read`, but `Iterator.next` does not allow it ``, noting "`E` is a type argument of `Iterator`: implement `Iterator` with `Read` as `E`" |

## 3. Generators (built, phase 2)

```ts
function* range(n: i64): Generator<i64> {
  for (let i = 0; i < n; i++) {
    yield i;
  }
}
```

- `function*` (and `*name()`, `static *name()`, `*[Symbol.iterator]()` methods) is a generator.
  Its declared result is `Generator<T, E>`, `Iterator<T, E>` or `Iterable<T, E>` (required); a
  written `E` is a `throws` clause, else `E` is inferred from the body like any `throws`, and a
  call has the declared type with that `E` (sema `collect/generator_sig.rs`,
  `body/generators.rs`; checked after inference like an async call's promise type). The call
  only creates the generator and never throws. `yield` is only allowed in generators (and not
  in an arrow inside one); `await` is not allowed in them; `return value` is an error (no
  `TReturn`); a bare `yield` needs `Generator<void>`.
- `Generator<T, E>` is a prelude **class** (`std/prelude/iter.vlt`) implementing
  `Iterator<T, E>` and `Iterable<T, E>`: `next()`, `return()`, `[Symbol.iterator]()` (returns
  `this` as an `Iterator<T, E>`) and `[Symbol.dispose]()` (closes it, so `using g = gen()`
  works and dropping it closes it). Its methods use the std-only intrinsics
  `__intrinsic_generator_resume` (throws `E`), `_value` and `_return`. `new Generator` and
  `extends Generator` are errors.
- `yield* src` is `for (const <yield@N> of src) { yield <yield@N>; }`: any iterable, and a
  direct generator call takes the embedded path below. Closing the outer generator while it is
  inside the loop leaves the loop early, which closes the inner iterator (`return()`, or the
  embedded state's close).

### Lowering (velt_vir `async_fn/generator.rs`, `async_fn/gen_object.rs`)

- A generator instance compiles like an async function: its poll function, built by the same
  code (`poll_fn` with `AsyncCx::generator`), is the **resume** function `f$poll(state, null)`
  — the `cx` parameter is kept so the liveness, spilling, frame-slot promotion (velt_opt
  `frame_slots`) and drop machinery apply unchanged — returning `0` DONE, `1` YIELDED (the value
  in the result region at offset 0: the `Ok` payload of `Result<T, E>` when the body throws,
  else `T`) or `2` THREW (the region holds `Err(e)`). Each `yield` stores the value, sets the
  tag to its suspension `k` and returns 1.
- **Closing** is the state's `$drop` (`Work::AsyncDrop`, unchanged: it sets `DROP_BIT` and calls
  the resume function). For a generator the dispatch case `DROP_BIT | k` of a `yield` runs what
  `return;` at that `yield` runs — every scope's drops *and `finally` blocks* — where async
  cancellation runs drops only. Before the first resume it drops the arguments; after the end
  it does nothing. Both leave the tag DONE, so `next()` after `return()` is done.
- Because closing runs `finally` blocks where the generator cannot pause or report an error,
  sema rejects `yield` in a `finally` block and a `finally` block that may throw.
- While the body runs the tag is RUNNING; resuming then (the body reaching its own generator)
  panics with `generator is already running` (JS throws a `TypeError`). The store is dead in
  an inlined loop and disappears.
- A **generator object** is one heap block `[table: ptr][state]` (the class's one field is the
  table pointer; states are at most 8-aligned): the static per-instance table holds resume,
  close and free functions. `Work::GenNew` builds it (counted like any object of the class when
  `Generator<T, E>` is shared), `Work::Fn` returns it or, for a declared `Iterator` / `Iterable`
  result, its interface value; the class's drop runs `[Symbol.dispose]` (close), then frees the
  block through the table (`object_free`). `console.log(g)` prints `Object [Generator] {}`
  like Node; a deep copy of one (`clone()`, a spawned task's copy) panics (sema already rejects
  `clone()` on types with `[Symbol.dispose]`).
- **Embedded loops**: `for (const x of gen(a))` with a direct call (sema emits `GeneratorEmbed`,
  hir_encodings.md "Generators") builds the state in a frame local (spilled into the enclosing
  state when the loop is in an async function or a generator, like an embedded awaited child),
  calls the resume function directly each step and reads the value slot; the local's drop
  closes it. A generator iterating a direct call of itself (recursion, `yield* walk(t.left)`)
  cannot embed its own state, so that call becomes a generator object.

### Cost

`bench/iter` (`bench/iter/run.sh`): 20 × 30M values summed modulo a prime, LLVM release, best
of 7 interleaved runs (Apple M4, shared with other builds: identical programs vary by up to
±10% between rounds):

| program | ms | vs hand loop |
|---|---|---|
| hand_loop (while loop) | 1172 | 1.00 |
| gen_loop (`for...of` over `range(n)`) | 1233 | 1.05 |
| iterable_class (iterator class) | 1147 | 0.98 |
| gen_value (`Iterable<i64>` parameter) | 2997 | 2.56 |

The embedded loop compiles to the hand loop's instructions (the same seven per value, rotated):
the resume function is inlined, the state is promoted to registers and the dispatch switch is
jump-threaded away; the difference in the table is noise. A loop the vectorizer can
handle as a hand loop (a filter into a plain sum) stays scalar as a generator, since the `yield`
is an exit from the producer's loop. A generator value costs an interface call and a table call
per step.

### Deviations from the accepted text

- `Generator<T, E>` is a class, not an interface: its `[Symbol.iterator]()` returns
  `Iterator<T, E>` (Velt has no covariant returns), and a generator value is the class's object.
- `resume` is the async poll function with a null `cx` (not a separate signature), and THREW is a
  third result.
- A `finally` block in a generator cannot `yield` or throw (TS allows both; see above).
- `for...of` over a generator *value* goes through the protocol (`[Symbol.iterator]()`, then
  `next()` through the interface); only direct calls are embedded.

## 4. Async generators and `for await` (phase 3, planned)

```ts planned
async function* lines(r: FileReader): AsyncGenerator<string, IoError> {
  while (true) {
    const line = await r.readLine();
    if (line == null) {
      return;
    }
    yield line;
  }
}
```

- `for await (const x of src)` works on `AsyncIterable<T, E>` in async code only (the diagnostic
  names the fix), and calls `await it.return()` on early exit, like section 2.
- An async generator's state machine has `poll(state, cx) -> { PENDING, YIELDED, DONE }`: `await`
  suspends with `PENDING` as today, and `yield` returns `YIELDED` with the value in the slot.
  A `for await` over a direct async generator call polls an embedded child state.

## 5. Std sources (phase 4, planned)

`Channel<T>` (#65) becomes `AsyncIterable<T>`: `for await (const job of jobs)` ends once the
channel is closed and drained. Then `FileReader` lines, WebSocket messages, Redis subscriptions and
`Ticker`.
