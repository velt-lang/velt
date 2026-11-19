# TypeScript alignment

Status: partly implemented. Issues: [#326](https://github.com/velt-lang/velt/issues/326), [#1546](https://github.com/velt-lang/velt/issues/1546).

## Summary

Velt introduces something that is not TypeScript only when the TypeScript way can't be compiled
at Rust speed or brings back a JavaScript bug source. Ordinary TypeScript compiles and means what
it means in TypeScript. There is no backward compatibility before 1.0: replaced syntax is
removed, not deprecated, and using it is an error with a fix.

This note covers the places where that principle shaped the language: inferred mutation,
discriminated unions, typed exceptions, `extend` (including retroactive `implements`) and a set
of smaller alignments. Strings, objects and numbers are in
[JavaScript semantics without a garbage collector](semantics.md) and
[JavaScript string semantics](strings.md).

## Design

### Mutation is inferred

Methods and parameters that may mutate on *any* path (any branch, case, loop, closure, callee or
override) are inferred as mutating. Object parameters follow JavaScript: mutations are visible to
the caller, and reassigning the parameter is local. Copy parameters (numbers, `bool`, Copy
structs) are passed by value. Exclusivity and `noalias` remain, driven by the inferred modes.
Writing `mut` before a parameter or method is the error "`mut` is not needed: mutation is
inferred".

### Discriminated unions and `switch`

```ts
type Shape = { kind: "circle"; r: f64 } | { kind: "rect"; w: f64; h: f64 };

function area(s: Shape): f64 {
  switch (s.kind) {
    case "circle":
      return Math.PI * s.r * s.r;
    case "rect":
      return s.w * s.h;
  }
}
```

- String, number and bool **literal types** (`"circle"`, `1`, `true`). Literal expressions take
  literal types only where one is expected (also for `const`).
- A union of object types sharing a literal-typed discriminant field compiles to a tagged union
  (the layout and speed of a Rust enum; the discriminant is the tag, not a stored string). Any
  object types can be members: anonymous object types, structs and classes.
- Narrowing through `switch (x.kind)` and `if (x.kind === "circle")`; fields every member has
  are readable without narrowing.
- Full JavaScript `switch`: fallthrough, `default`, `break`, blocks; each `case` body is its own
  scope; case values that are not literals compare with `==`. Exhaustiveness is checked when
  switching on a discriminant or a union (`never` in `default`).
- There are no payload enums and no `match`: `enum` members cannot have payloads, and `match` is
  "`match` is not supported". TypeScript's numeric enums (`enum Color { Red, Green = 5 }`) and
  string enums (`enum Dir { Up = "UP" }`) work as in TypeScript.

The rules are in [the Reference](https://veltlang.com/docs/reference/types/#discriminated-unions).

### Errors: typed `throw`, `try` and `catch`

- In `catch (e)`, `e` is the exact union of the error types the `try` block can throw; narrow it
  with `instanceof` or `switch`. Exhaustiveness of narrowing chains is checked.
- A `throws` clause follows the return type and is optional: `function f(): T throws A | B`,
  also on methods, constructors, arrows (`(x): R throws E => …`) and
  `async function f(): Promise<T> throws E`. It is inferred when omitted; when written, the body
  must not throw anything else. Function types are written `(x: T) => U throws E`; after a
  `Promise` result, `throws` is the promise's rejection type, and promise values are
  `Promise<T, E>`.
- Higher-order functions propagate callback errors by being generic over them
  (`map<U, E>(f: (x: T, i: i64) => U throws E): U[] throws E`), as the prelude's array, `Map`
  and nullable callbacks and the std collections do. Interface and overridden methods share one
  error type (the declaring method's clause, else the union of the implementations').
- Errors as values: union returns (`User | NotFound`) with narrowing, and `attempt(() => f())`,
  which gives `T | E`.
- Async matches JavaScript: awaiting a spawned task or `Promise.all` rethrows the error, typed;
  `Promise.all` rejects with the first rejection as soon as it happens; a `spawn(...)` statement
  reports its error as uncaught.
- Panics (bounds, division by zero, `x!` of `null`) are separate from throws.

The rules are in the Reference's [Errors](https://veltlang.com/docs/reference/errors/) and
[Async](https://veltlang.com/docs/reference/async/) pages.

### `extend`

- **Members on any type**: methods, getters, setters and static methods, on built-in types
  (`string`, numbers, arrays, `Map`, `Promise`), classes, structs and unions, including
  discriminated unions (`extend Shape { area(): f64 { switch … } }`).
- **Blanket extensions**: `extend<T extends Comparable<T>> T { clamp(lo: T, hi: T): T { … } }`;
  extending through a bound adds the method to every implementor.
- **Overlapping extensions**: among the applicable blocks (target matches, bounds hold) the most
  specific wins: A over B when A's target is an instance of B's and not the reverse
  (`extend Array<i64>` over `extend<T> Array<T[]>` over `extend<T> Array<T>`), or when both have
  the same target and only A has bounds. Otherwise the call is an error,
  ``ambiguous extension method `m` on `T` ``, naming both blocks.
- A type's own member always wins over an extension. An extension adds no fields (the layout is
  fixed).
- **Operators**: operator methods keyed by well-known symbols (`[Symbol.add](other)` for
  `a + b`, [operators.md](operators.md)); an `extend` block may add them to a class, struct or
  vector, never to a type whose operators are built in.
- An extension applies wherever its module is loaded; prelude extensions are global.

### Retroactive `implements`

`extend X implements I { … }` gives `I` to types their authors did not write it for, for any
type and any interface without fields. Blanket blocks (`extend<T extends B> T implements I`)
give `I` to every type with `B`, type parameters included, so generic code can be specialized
for some types at no run-time cost (`crates/velt_sema/src/collect/ext_impls.rs`). The rules, and
why:

- **Own implementations first.** A type's own implementation (a class's `implements`, one it
  inherits from a base class, or the implicit `Comparable`/`Iterable` of a `compareTo` or
  `[Symbol.iterator]()` method) wins over every `extend` block, as a type's own member wins over
  an extension method. A blanket block applies only to types without one, so a subclass never
  loses its base class's or its own override. A block for a type that already has one is
  refused.
- **One most specific block.** Among the blocks that fit a type, the most specific wins, by the
  overlap rule above. Two blocks that can fit one type where neither is more specific are
  refused where they are declared, not where a type is used: generic code is checked once and
  instantiated with many types, so a check at the use (`Pair<A, i64>` in a generic function)
  would miss the instance where both fit (`Pair<i64, i64>`), and the choice would depend on
  declaration or import order. The check ignores bounds: two blocks with the same target, both
  bounded, are refused even if no type meets both bounds.
- **Coherence, without an orphan rule.** Any module may implement any interface for any type, so
  two libraries that each write `extend u64 implements I` for the same `I` conflict in every
  program that loads both. The whole program is compiled together, so this is a build error
  naming both blocks, never different behavior in different places. The convention that avoids
  it is the orphan rule as advice: a library implements its own interfaces for any type, or any
  interface for its own types.
- **Program-wide.** A type has the same implementation of `I` everywhere, since generic code is
  instantiated per type, not per importing module, and an `I` value made in one module is used
  in others.
- **Refused targets.** A block cannot target `any`, `unknown`, `object`, `Function` or `never`:
  they have no implementation of their own, and an `any` holds a value of another type
  (converting an `any` to `I` uses the held value's implementation). A union target is refused,
  since a union's value has one member's type. An interface type implementing an interface it
  extends is refused as already implemented.
- **Bounds hold when they hold at all.** A block's bounds are checked through implementations
  again, so blocks can lead back to each other (`extend<T extends Q> T implements P`,
  `extend<T> T implements P` and `extend<T extends P> T implements Q`). A bound holds when the
  type meets it through its own implementation or through a finite chain of blocks, never
  through itself: the least answer, the same in every context and every order. With the three
  blocks above, `C` is a `P` through the unbounded block, hence a `Q`, hence the bounded `P`
  block fits and wins. A pair that only leads to itself (`extend<T extends I> T implements J`
  with `extend<U extends J> U implements I`) gives a type neither interface unless it has one of
  its own. A blanket block bounded by the interface it implements could only apply to types that
  already have it, and is refused.
- **Interface arguments.** A type implements an interface once, as a class does: blocks that can
  fit one type conflict whatever their interface arguments (`extend W implements G<i64>` and
  `extend W implements G<string>`), and the block that wins decides the arguments. This keeps
  sema's lookup (by interface) and lowering's (by interface and arguments) the same. A block
  with bounds must give the arguments of the blocks it takes precedence over; otherwise whether
  a type is a `G<string>` could depend on the bounds of a `G<i64>` block that depend on
  `G<string>` in turn, with no consistent answer. Such a pair is refused where it is declared. A
  more specific block without bounds may give other arguments (`extend<T> Box<T> implements
  G<T>` over `extend<A> A implements G<i64>`): it always wins for the types it fits.
- **Bounds.** A block that lists `implements` takes interface bounds only; `any`, `unknown`,
  `object` and `Function` bounds are refused (the first two add nothing, the last two would need
  dynamic type tests in every lookup). Blanket blocks apply to type parameters through each
  other, and an interface value meets a bound on its interface or one it extends.

### Smaller alignments

- **Scripts.** The root file's statements outside declarations run in order in a generated
  `main` (`async` when one awaits), as in a TypeScript file. Top-level `const`/`let`
  declarations move into it unless a declaration refers to them; those stay module constants.
  An imported module's statements run once when the module is evaluated, in Node's module
  order (`crates/velt_sema/src/module_scripts.rs`). In a root file that also declares
  `function main()`, the generated function is named `#main` (`velt_syntax::SCRIPT_MAIN`) and is
  the entry point, and `main` is an ordinary function, as in Node.
- **Callbacks.** The prelude's array callbacks take the index (`(x, i)`, `reduce`'s
  `(acc, x, i)`). An arrow may take fewer parameters than the function type it is passed as,
  and a named function with fewer parameters is wrapped. The array is not passed as a third
  argument: calling an unknown callback with the receiver would count as modifying it and make
  every `forEach` require mutable access.
- **Parameters.** Arrow defaults and optional parameters, rest parameters (a call packs the
  remaining arguments, spreads included, into an array), and constructor parameter properties
  (`constructor(private readonly name: string, public age: i64) {}` declares and assigns the
  fields).
- **Syntax.** `a ??= b`, `a ||= b` and `a &&= b`; `x!`, which Velt checks (`x ?? panic(…)`) where
  TypeScript trusts it; defaults in destructuring patterns; `boolean` as an alias of `bool`.
- **Built-ins.** `s[i]` is `s.charAt(i)`; `Math.random()`; `Date` (`std/prelude/date.velt`) with
  JavaScript's months 0-11 and local-time getters; `<` on a class implementing `Comparable`
  calls `compareTo`; a template literal calls a value's own `toString(): string`.
- **Sorting.** `arr.sort(cmp?)` takes an optional comparator like TypeScript, and with one the
  sort is stable. Without one it is defined on arrays of numbers and strings and sorts numbers
  numerically, not in JavaScript's string order.
- **`m[i, j]`** is an operator indexer with several keys ([operators.md](operators.md)). In
  TypeScript `a[i, j]` is the comma operator: it reads `a[j]` and evaluates `i` for its effects,
  and `tsc` rejects it only when `i` has none (TS2695). On arrays and the other built-in types it
  is an error with the fix `a[i][j]` rather than a different meaning, and `--ts-compat` reports
  every use (rule `operator`).

## Compilation

- Discriminated unions lower to tagged unions in VIR; the discriminant field is the tag.
- Throwing functions return results: one branch per throwing call, no unwinding.
- Extension methods are resolved statically; a blanket block is monomorphized like a generic
  function. Retroactive implementations are entries in `Program::impls`, chosen by
  `Ctx::find_impl` (`crates/velt_sema/src/infer.rs`, `infer/ext_impls.rs`) and by
  `crates/velt_vir/src/lower/impl_lookup.rs`.
- A bound check through implementations is not a nested call: it collects every lookup the
  question reaches (finitely many, since a bound is checked on a type argument of the type
  looked up) and iterates to a fixed point, in sema (`crates/velt_sema/src/infer/ext_exists.rs`)
  and in lowering (`crates/velt_vir/src/lower/ext_exists.rs`). Every answer is remembered, and a
  chain of blocks whose bounds share interfaces is looked up once per type. What a type
  parameter gets from blanket blocks is found with a worklist, so a chain costs one look per
  block in any declaration order.

## Diagnostics

- "`mut` is not needed: mutation is inferred".
- "`match` is not supported"; "enum members cannot have payloads".
- ``ambiguous extension method `m` on `T` ``.
- `` `P` already implements `I` ``.
- ``conflicting implementations of …: neither block is more specific than the other``, and
  ``conflicting implementations of `I` and `J`: the block for `T` has bounds and takes precedence
  over …``.
- ``` `extend X` cannot implement `I`: … ```, for an interface with fields (an `extend` block
  cannot add fields) and for the refused targets.
- ``` `extend X` implements `I` only for types that already implement it ```.
- For `a[i, j]` on a built-in type: "to index an element of an element, write `a[i][j]`".

## Compatibility

- `catch (e)` is typed with the thrown union rather than `unknown`.
- `x!` is checked at run time.
- `sort()` without a comparator orders numbers numerically, and `sort` returns nothing.
- `a[i, j]` on built-in types is an error rather than the comma operator.

## Not yet implemented

- **Module-scoped extensions.** An extension is visible in its module and where it is imported
  (`import { Stats } from …`); prelude extensions stay global. `implements` blocks stay
  program-wide (see "Retroactive `implements`"); their methods called as extension methods
  follow the scoping rule.
- **`static readonly` constants in `extend` blocks.**
