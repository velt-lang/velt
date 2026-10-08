# Design: TypeScript compatibility, round 1

Status: implemented. Tracking issue: #326 (TypeScript friction). Related: #214 (lengths and
sizes).

Principle (from [TypeScript alignment](ts-alignment.md)): ordinary TypeScript should compile and
mean what it means in TypeScript, unless that costs native speed or brings back a JavaScript bug
source. This round removes the errors a TypeScript developer meets in a first small program (a
20-line numeric exercise hit three before it ran) and the differences that compiled silently.

## 1. Numbers from the standard library are JS numbers

**Problem.** `for (let i = 0; i < xs.length; i++)` was `expected i64, found usize`;
`xs.length / 2` truncated although the user wrote no integer type; `xs[i]` with `i: number`
was an error; `s.slice(0, s.length - 1)` mixed `usize` and `i64`.

**Semantics.** Every integer is *declared* (its type is written) or *inferred* (a JS number held
as an integer, [Numbers](../../reference/types.md#numbers)). Inferred now also covers integers the
standard library hands to user code: `ArrayLen` / `StrLen`, results of calls to `std/` functions
and methods (`indexOf`, `size` getters, `Date.now()`), bindings destructured from such results
(the `entries()` index), unannotated integer parameters of callbacks passed to `std/` functions,
and fields declared from an integer literal (`count = 0`). Inside `std/` nothing changes, so the
library keeps Rust-style integer code. Then:

- An inferred integer next to an integer of another type adapts: a declared type other than
  `usize` wins, otherwise both become `i64` (`let i = -1; i < xs.length` is `true`). Where an
  integer type is expected (arguments, returns), it converts to it. In a compound assignment
  (`total += s.length`) the target keeps its type, so the value converts to it when either side
  is inferred (#421); a signed value never converts to an unsigned target. A local declared
  without a type from such a value (`let n = xs.length`) is an `i64`, so it can go negative.
- A float index converts through the prelude's `__floatIndex`: whole numbers index, anything else
  panics like an out-of-bounds index. A quotient index (`xs[n / 2]`) stays an error.
- A float argument for an integer parameter of a `std/` function converts like
  `ToIntegerOrInfinity` (a saturating cast): `xs.slice(0, xs.length / 2)`.

**How it compiles.** No new types or HIR: lengths stay `usize` values. The origin is computed
from the HIR (`numbers::int_origin`); a conversion the compiler inserts reuses its operand's
span, which tells it apart from a written `as`, so it keeps the operand's origin.

**Cost.** None on integer paths: the conversions are the casts the user would have written.

**Migration.** `const k: usize = xs.length; k / 2` keeps truncating (declared). Code that relied
on `xs.length / 2` truncating gets a compile error where an integer is required, with the hint
`Math.trunc(a / b)`. The golden `lang/numbers_division` was updated for the new results.

## 2. Scripts: top-level statements

The root file's statements outside declarations run in order in a generated `main` (`async` when
one awaits), like a TS file. Top-level `const`/`let` declarations move into it unless a
declaration refers to them (a scope-aware scan of free names, `parser::script_names`); those stay
module constants. The parser builds the `main` (its name has an empty span); sema rejects
statements in an imported module; `velt fmt` prints the statements back at the top level. A file
cannot have both top-level statements and `function main()`.

## 3. Functions

- **Callbacks get the index**: the prelude's array callbacks take `(x, i: i64)` (`reduce`:
  `(acc, x, i)`). As in TS, an arrow may take fewer parameters than the function type it is passed
  as (the closure gets unnamed extra parameters), and a named function with fewer parameters is
  wrapped in an arrow that passes the leading arguments. The array is not passed as a third
  argument: calling an unknown callback with the receiver would count as modifying it and make
  every `forEach` require mutable access.
- **Arrow defaults and optional parameters** (`ast::ArrowParam::{default, optional}`): defaults
  are checked like a function's (module scope) and stored on the closure; a call through a
  `const` bound to the arrow fills in left-out arguments. Trailing defaulted parameters beyond an
  expected function type become locals.
- **Rest parameters** (`ast::Param::rest`, default `[]`): a call packs the remaining arguments,
  spreads included, into an array literal. A spread before the rest position is an error, except
  for `std/` functions whose skipped parameters have defaults (`Math.max(a = -Infinity, b =
  -Infinity, ...rest)`), where the defaults are identities. The two-argument `Math.max` passes an
  empty array, which allocates nothing (measured: no slower than a hand-written max).
  `splice` and `toSpliced` take the items to insert as a rest parameter.

## 4. Syntax

- `a ??= b`, `a ||= b`, `a &&= b` desugar to `a = a ?? b` (and so on): the target is a place.
- `x!` (`ast::ExprKind::NonNull`) is `x ?? panic(…)`: TypeScript trusts the assertion, Velt
  checks it.
- `e as const` (a cast to the type named `const`) is the value unchanged.
- Defaults in `const`/`let` patterns (`ast::PatternKind::Default`) desugar into declarations
  through hidden temporaries: a field's default applies when it is `null`, an element's when the
  array is too short. Tuples keep plain destructuring (their elements always exist).
- `boolean` is an alias of `bool`.

## 5. Built-ins

- `s[i]` is `s.charAt(i)`; `charAt` and `at` are new; `for...of` over a string iterates its code
  points. Positions count UTF-16 code units since #377 phase 2b
  ([design/strings.md](strings.md)).
- `Math.random()`.
- `Date` (prelude, `std/prelude/date.vlt`): JavaScript's, on velt:datetime, with months 0-11 and
  local-time getters. `console.log` prints a `Date` through its `__inspect()` (the ISO string, as
  Node). Loading it costs about 7 ms of checking per program (velt:datetime's modules join the
  prelude).
- `<` on a class or struct implementing `Comparable` calls `compareTo` (it did only on type
  parameters), and a template literal calls a value's own `toString(): string`.

## Fixed on the way

Array spread did not check the source's element type (`const ys: string[] = [...nums]` pushed
integers into a string array); integers now convert into a float array and other mismatches are
errors.

## Not in this round

Structural interfaces, utility types, `readonly` object-type fields, JSON output of absent
optional fields, `velt check --ts-compat` (the lint for code shared with `tsc`: see
[TSX, "The common subset"](tsx.md#the-common-subset)) and the `.d.ts` package (shared models and
tooling, in separate work); UTF-16 string positions; overloads.
