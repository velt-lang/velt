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
- **Retroactive implements** (implemented, #1546): `extend Point implements Comparable<Point> { … }`,
  for any type and interface without fields. Blanket blocks (`extend<T extends B> T implements I`)
  give `I` to every type with `B`, type parameters included, and the most specific block that fits
  a type is its implementation (the overlap rule below), so generic code can be specialized for
  some types at no run-time cost (`velt_sema` `collect/ext_impls.rs`).
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

### Retroactive implements: the rules

`extend X implements I` (#1546, PR #1548) gives `I` to types their authors did not write it for.
The rules, and why:

- **Own implementations first.** A type's own implementation (a class's `implements`, one it
  inherits from a base class, or the implicit `Comparable`/`Iterable` of a `compareTo` or
  `[Symbol.iterator]()` block) wins over every `extend` block, as a type's own member wins over
  an extension method. A blanket block (`extend<T> T implements I`) applies only to the types
  without one, so a subclass never loses its base class's or its own override. A block for a
  type that already has one is refused (`` `P` already implements `I` ``).
- **One most specific block.** Among the blocks that fit a type (target pattern matches,
  bounds hold), the most specific wins, by the overlap rule above. Two blocks that can fit one
  type where neither is more specific are refused where they are declared (``conflicting
  implementations of `I` ``, naming a type both fit), not where a type is used: generic code is
  checked once and instantiated with many types, so a check at the use (`Pair<A, i64>` in a
  generic function) would miss the instance where both fit (`Pair<i64, i64>`), and the choice
  would depend on declaration or import order. The check ignores bounds: two blocks with the
  same target, both bounded, are refused even if no type meets both bounds.
- **Coherence, without an orphan rule.** Any module may implement any interface for any type,
  so two libraries that each write `extend u64 implements I` for the same `I` break every
  program that loads both: the program gets the conflict error and does not build. The whole
  program is compiled together, so this shows up at build time with both blocks named, never
  as different behaviour in different places. The convention that avoids it is the orphan rule
  as advice: a library implements its own interfaces for any type, or any interface for its own
  types. A lint for it may come later.
- **Scoping.** Today a block applies wherever its module is loaded. When extension methods
  become module-scoped (above), `implements` blocks stay program-wide: a type must have the
  same implementation of `I` everywhere, since generic code is instantiated per type, not per
  importing module, and an `I` value made in one module is used in others. The block's methods
  called as extension methods follow the scoping rule.
- **`any` and other targets.** A block cannot target `any`, `unknown` (or `object`,
  `Function`) or `never`: they have no implementation of their own, and an `any` holds a value
  of another type. Converting an `any` to `I` uses the held value's implementation, by the same
  lookup as a direct conversion. A union target is refused too: a union's value has one
  member's type, which the block's target does not match. An interface type implementing an
  interface it extends is refused as already implemented.
- **Bounds hold when they hold at all.** A block's bounds are checked through implementations
  again, so blocks can lead back to each other (`extend<T extends Q> T implements P`, `extend<T>
  T implements P` and `extend<T extends P> T implements Q`). A bound holds when the type meets
  it through its own implementation or through a finite chain of blocks, whichever block that
  is, never through itself: the least answer, which is the same in every context. (The first
  rule, "a lookup that comes back to itself does not fit", made the answer depend on which
  lookup ran first once lookups were remembered.) With the three blocks above, `C` is a `P`
  through the unbounded block, hence a `Q`, hence the bounded `P` block fits and wins, in every
  order. A pair that only leads to itself (`extend<T extends I> T implements J` with
  `extend<U extends J> U implements I`) gives a type neither interface unless it has one of its
  own. A blanket block bounded by the interface it implements could only apply to types that
  already have it, and is refused.
- **Interface arguments.** A type implements an interface once, as a class does: blocks that
  can fit one type conflict whatever their interface arguments (`extend W implements G<i64>`
  and `extend W implements G<string>`), and the block that wins decides the arguments. This
  keeps sema's lookup (by interface) and lowering's (by interface and arguments) the same. A
  block with bounds must give the arguments of the blocks it takes precedence over: those it is
  more specific than, for the types both fit, and those for a base class of a type it fits.
  Otherwise whether a type is a `G<string>` would depend on the bounds of a `G<i64>` block,
  and those can depend on `G<string>` in turn (`extend<T extends Q> T implements G<i64>`,
  `extend<T> T implements G<string>`, `extend<T extends G<string>> T implements Q`): no answer
  is consistent, and checking "some block for `G<string>` fits" would let one type have two
  implementations. Such a pair is refused where it is declared. A more specific block
  without bounds may give other arguments (`extend<T> Box<T> implements G<T>` over `extend<A>
  A implements G<i64>`): it always wins for the types it fits, so a `Box<string>` meets a
  `G<string>` bound and not a `G<i64>` one, and the blocks below it don't apply.
- **Bounds.** A block that lists `implements` takes interface bounds only. `any`, `unknown`,
  `object` and `Function` bounds are refused: the first two add nothing, and the last two
  would need the dynamic type tests of `any` in every impl lookup. Blanket blocks apply to
  type parameters through each other (a `T extends K` gets `J` from `extend<T extends K> T
  implements J`, then `I` from `extend<T extends J> T implements I`). An interface value meets
  a bound on its interface or one it extends, as for a generic function, so `extend<T extends
  P> T implements Q` gives a `P` value `Q`.
- **Cost.** A bound check is not a nested call: it collects every lookup the question reaches
  (finitely many, since a bound is checked on a type argument of the type looked up) and
  iterates to a fixed point, in sema (`infer/ext_exists.rs`) and in lowering
  (`lower/ext_exists.rs`). There is no nesting limit, every answer is remembered, and a chain
  of blocks whose bounds share interfaces is looked up once per type. One computation starts
  another only through a structural implementation (an intersection bound's), made at most
  once per type and interface, so they nest finitely. What a type parameter gets from blanket
  blocks is found with a worklist: a bound it gets looks again only at the blocks bounded by
  that interface or one it extends, so a chain costs one look per block in any declaration
  order (300 levels with calls through `this` in every body: 0.16 s to check).
- **Diagnostics**: a missing or mismatched method (as for classes); `conflicting
  implementations`; `already implements`; ``cannot implement `I` `` for an interface with
  fields or optional methods (an `extend` block cannot add fields) and for the targets above;
  ``implements `I` only for types that already implement it``.

## 5. Small TypeScript alignments (implemented)

- `arr.sort(cmp?)` takes an optional comparator like TypeScript; `sortBy` is removed (one way).
  Without a comparator it sorts by `Comparable` order (numbers numerically, not JavaScript's
  string order).
- Constructor parameter properties:
  `constructor(private readonly name: string, public age: i64) {}` declares and assigns the
  fields.
