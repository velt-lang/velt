# Design: TypeScript alignment

Status: decided. Sections 1–3 and 5 are implemented; section 4 is partly implemented.

Principle: **introduce something that is not TypeScript only when it can't be expressed the
TypeScript way at Rust speed.** There is no backward compatibility before 1.0: old syntax is
removed, not deprecated.

## 1. `mut` removed: mutation is inferred (implemented)

Methods and parameters that may mutate on *any* path (any branch, case, loop, closure, callee or
override) are inferred as mutating. Object parameters follow JS semantics: mutations are visible
to the caller, and reassigning the parameter is local. Copy parameters (numbers, `bool`, Copy
structs) are always passed by value. Exclusivity and `noalias` remain, driven by the inferred
modes. Writing `mut` is an error with a fix.

## 2. Discriminated unions and `switch` replace payload enums and `match` (implemented)

```ts ignore
type Shape = { kind: "circle"; r: f64 } | { kind: "rect"; w: f64; h: f64 };
switch (s.kind) { case "circle": return s.r; case "rect": return s.w * s.h; }
```

- String, number and bool **literal types** (`"circle"`, `1`, `true`).
- A union of object types sharing a literal-typed discriminant field compiles to a tagged union
  (the same layout and speed as a Rust enum; the discriminant is the tag, not a stored string).
- Narrowing through `switch (x.kind)` and `if (x.kind === "circle")`.
- Full JavaScript `switch`: fallthrough, `default`, `break`, blocks. Exhaustiveness is checked
  when switching on a discriminant or a union (`never` in `default`).
- Payload `enum`s and `match` are removed. TypeScript's numeric enums (`enum Color { Red,
  Green = 5 }`) and string enums (`enum Dir { Up = "UP" }`) stay.
- As built ([the Reference](../../reference/types.md#discriminated-unions)): any object types
  can be members (anonymous object types, structs, classes); literal expressions take literal
  types only where one is expected (also for `const`); each `case` body is its own scope; case
  values that are not literals compare with `==`; fields every member has are readable without
  narrowing.

## 3. Errors: typed, checked `throw`/`try`/`catch`; `Result`, `Ok`, `Err` and `?` removed (implemented)

- In `catch (e)`, `e` is the exact union of the error types the `try` block can throw; narrow it
  with `instanceof` or `switch`; exhaustiveness of narrowing chains is checked.
- A `throws` clause on signatures is optional (`function f(): T throws A | B`) and inferred when
  omitted; when written, the body must not throw anything else. Editors show which calls can
  throw.
- Errors as values: union returns (`User | NotFound`) and narrowing; `attempt(() => f())` gives
  `T | E`.
- Async matches JavaScript: awaiting a spawned task or `Promise.all` rethrows the task's error,
  typed.
- The cost model is unchanged: result-style returns, one branch per throwing call, no unwinding.
- Hot reload constraint ([hot reload](hot-reload.md)): futures keep their own `poll`/`drop` in
  the future header; no other place caches code pointers.
- Panics (bounds, division by zero, `unwrap` of `null`) stay separate from throws.
- As built ([Errors](../../reference/errors.md), [Async](../../reference/async.md)): `throws`
  follows the return type (`function f(): T throws E`, also on methods, constructors, arrows
  `(x): R throws E => …`, and `async function f(): Promise<T> throws E`); function types
  `(x: T) => U throws E`, where `throws` after a `Promise` result is the promise's rejection
  type; promise values are `Promise<T, E>`. Higher-order functions propagate callback errors by
  being generic over them (`map<U, E>(f: (x: T) => U throws E): U[] throws E`), as the prelude's
  array, `Map` and nullable callbacks and the std collections do. Interface and overridden
  methods share one error type (the declaring method's clause, else the union of the
  implementations'). `Promise.all` rejects with the first rejection as soon as it happens, like
  JS; a `spawn(...)` statement reports its error as uncaught; `serve` answers 500 for a
  throwing handler.

## 4. `extend` — full power, zero cost, module-scoped

- **Members on any type** (implemented): methods, getters and setters, and static methods, on
  builtins (`string`, numbers, arrays, `Map`, `Promise`), classes, structs and unions.
  Planned: `static readonly` constants.
- **Methods on unions** (implemented), including discriminated unions:
  `extend Shape { area(): f64 { switch … } }`.
- **Blanket extensions** (implemented): `extend<T extends Comparable<T>> T { clamp(lo: T, hi: T):
  T { … } }`; extending through a bound adds the method to every implementor.
- **Retroactive implements** (planned): `extend Point implements Comparable<Point> { … }`.
- **Overlapping extensions** (implemented): among the applicable blocks (target matches,
  bounds hold) the most specific wins, A over B when A's target is an instance of B's and not
  the reverse (`extend Array<i64>` over `extend<T> Array<T[]>` over `extend<T> Array<T>`), or
  when both have the same target and only A has bounds.
  Otherwise the conflicting applicable extensions are an ambiguity error at the call
  (``ambiguous extension method `m` ``, naming both blocks).
- **Scoping** (planned): an extension is visible in its module and where it is imported
  (`import { Stats } from …`); prelude extensions are global. Today an extension applies
  wherever its module is loaded.
- A type's own member always wins over an extension. No new fields (the layout is fixed). No
  operator overloading.

## 5. Small TypeScript alignments (implemented)

- `arr.sort(cmp?)` takes an optional comparator like TypeScript; `sortBy` is removed (one way).
  Without a comparator it sorts by `Comparable` order (numbers numerically, not JavaScript's
  string order).
- Constructor parameter properties:
  `constructor(private readonly name: string, public age: i64) {}` declares and assigns the
  fields.
