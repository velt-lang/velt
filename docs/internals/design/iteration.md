# Design: iteration, generators and `for await`

Status: accepted (issue #62), implemented in four phases. **Phase 1 (the protocol and `for...of`
over user iterables) is built**; phases 2–4 below are the plan and are updated as they land.

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
- **Phase 2 hook**: `for_of_iterable` receives the checked `src`; a fast path for a direct
  generator call (`for (const x of gen(args))`) goes before the `[Symbol.iterator]()` call and
  emits a dedicated loop over the embedded generator state instead of the protocol.

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

## 3. Generators (phase 2, planned)

```ts planned
function* range(n: i64): Generator<i64> {
  for (let i = 0; i < n; i++) {
    yield i;
  }
}
```

- `function*` returns `Generator<T, E>`, which is `Iterable<T, E>` and `Iterator<T, E>`; `E` is
  inferred from the body like any `throws`. `yield` is only allowed in generators; `yield*`
  delegates to another iterable.
- Lowering reuses the async state-machine transform (`velt_vir/src/lower/async_fn/`): every
  `yield` is a suspension point, live locals are spilled into the state with the same liveness
  and drop-at-suspension code; the state has a value slot that `yield` writes, and
  `resume(state) -> { YIELDED, DONE }`.
- **Early exit**: `return()` on a generator suspended at a `yield` resumes it as if the `yield`
  were a `return`: `finally` blocks run and `using` resources are disposed. Dropping a suspended
  generator does the same (it is the state's `$drop`, as async cancellation is today), so
  `using it = gen()` and dropped generators never leak.
- **Performance target**: `for (const x of gen(args))` embeds the generator state in the caller's
  frame (as `await f()` does): no allocation for the generator or per item; the loop calls the
  resume function directly and reads the value slot, never building an `IteratorResult`. A
  generator stored or passed as an `Iterable<T>` is boxed once. A `bench/` entry compares it with
  a hand-written loop (target: within a few percent).

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
