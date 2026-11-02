# Design: operator overloading

Status: **accepted** (issue #2050, maintainer review 2026-11-02, its points written in below);
built by a stack of five pull requests (steps 1–2, 3–4, 5, 6 and 7 below); step 8 (`a[i, j]`)
waits for its `ast::ExprKind::Index` change. Area 2 of #1866.

## Problem

Numeric code on user types reads as method chains: `a.add(b.mul(0.5)).sub(c.neg())` for what
the math writes `a + b * 0.5 - -c`. Matrix, vector, complex-number, big-integer and SIMD code is
the first thing a Python, Julia or Mojo audience writes, and the first that reads badly.

Goal: `+ - * / % **`, the bitwise operators, unary `-` and `~`, compound assignment and `[]`
(and `a[i, j]`) work on classes, structs, `velt/simd` vectors and type parameters. Each operator
resolves at compile time to one method call, monomorphized and inlined: no runtime lookup and no
cost over the method it calls.

## Where Velt was (main `8db51d1a5`)

- `a + b` on a class gave ``cannot apply binary operator `+` to type `V` ``; on a vector the note
  "SIMD values have methods instead of operators: `a.add(b)`".
- **Two operators already lowered to methods:**
  1. `<`, `<=`, `>` and `>=` on a `Comparable` type are `a.compareTo(b) < 0`, a
     `Callee::ParamMethod` with both operands borrowed (`ordering.rs`).
  2. `xs[i]`, `xs[i] = v`, `xs[i] op= v` and `xs[i]++` on the typed arrays and `RegExpMatch`
     called `[INDEX](i)` / `[SET_INDEX](i, v)` with a read-then-write rewrite evaluating each
     part once (`indexers.rs`), limited to std classes by std's private keys.

  This design generalizes those two mechanisms instead of adding a third.
- std spelled operators as methods: `velt/math`'s `BigInt` (`add` … `neg`, and in-place
  `addAssign` … `shrAssign`), `Duration` (`plus`, `minus`, `negated`), `velt/simd`'s ~450 lane
  operations.
- Classes behave as in JS: `a.f(b)` leaves `b` usable and `m.at(0).x = 10` changes the element,
  so an operator can mean exactly the call it stands for, with no new ownership rule.
- **Free syntax and semantics:** `a[i, j]` was a parse error (Velt has no comma operator), and
  arithmetic, bitwise and unary operators on objects, `obj[i]` without an index signature and
  `a[i, j]` are type errors in TypeScript. Giving them a meaning changes no valid TypeScript
  program.

## Model

**An operator is a method with a well-known symbol key.** Declaring one opts a type in:

```ts
class Vec3 {
  constructor(public x: f64, public y: f64, public z: f64) {}

  [Symbol.add](o: Vec3): Vec3 { return new Vec3(this.x + o.x, this.y + o.y, this.z + o.z); }
  [Symbol.mul](k: f64): Vec3 { return new Vec3(this.x * k, this.y * k, this.z * k); }
  static [Symbol.mul](k: f64, v: Vec3): Vec3 { return v * k; }   // `2 * v`
  [Symbol.neg](): Vec3 { return new Vec3(-this.x, -this.y, -this.z); }
}

const p = a + b * 0.5 - -c;     // a[Symbol.add](b[Symbol.mul](0.5))[Symbol.sub](c[Symbol.neg]())
```

`a + b` is exactly `a[Symbol.add](b)`, which may also be called directly. Lookup is a method
call's: own and inherited methods (virtual when overridden), `extend` blocks (their specificity
and ambiguity rules), interface values and the bounds of a type parameter. Operands are passed
as that call passes arguments, they throw what it throws (`throws E` is allowed), and they are
evaluated left to right.

Symbol keys rather than names (Kotlin's `plus`): a name would turn existing methods into
operators (`Set.add(x)` must not make `set + x` compile). `compareTo` stays the name behind `<`,
a protocol with one meaning that already shipped. The keys are TypeScript syntax (computed keys,
like `[Symbol.iterator]`) and live on `Symbol` with Velt's other well-known symbols.

### The operators

| Operator | Method | Shape |
|---|---|---|
| `a + b`, `-`, `*`, `/`, `%`, `**` | `[Symbol.add]`, `sub`, `mul`, `div`, `rem`, `pow` | `(other: R): O` |
| `a & b`, `\|`, `^`, `<<`, `>>`, `>>>` | `[Symbol.bitAnd]`, `bitOr`, `bitXor`, `shl`, `shr`, `ushr` | `(other: R): O` |
| `l op b`, only `b` of the type | `static [Symbol.<op>](l: L, b: Self): O` | the first parameter not the declaring type |
| `-a`, `~a` | `[Symbol.neg]`, `[Symbol.bitNot]` | `(): O` |
| `a op= b` | `[Symbol.<op>Assign]` (`addAssign` … `ushrAssign`) | `(other: R): void`, changes `this` |
| `a[i]` | `[Symbol.index]` | `(i: K, …): V` |
| `a[i] = v` | `[Symbol.setIndex]` | `(i: K, …, v: V): void` |

**Not overloadable:** `===`/`!==` (identity); `==`/`!=` (identity on classes as in Node, content
on structs with #641); `<` … `>=` (`Comparable`); `&&`, `||`, `??`, `!`, `in`, `instanceof`,
`typeof`, unary `+` (ToNumber). `++`/`--` are `+= 1`/`-= 1`.

**An operator method never changes what an existing program means.** It is consulted only where
the operand types have no built-in meaning for the operator: never for numbers, `string`,
`bool`, `any`/`unknown`, arrays, tuples, records or `Map` indexing. User code declaring one on
such a type is an error; std does, for generic code (below).

- **`+` with a `string` operand is always concatenation**, as in TypeScript, so a `[Symbol.add]`
  taking a `string` (or a static one whose first parameter is a `string`) is unreachable and an
  error.
- **`any` and `unknown` stay JS:** an object with `[Symbol.add]` held as `any` gets JS's
  ToPrimitive, never the method.

### Resolution of a binary operator

For `a op b` with operand types `A` and `B`:

1. The built-in meaning if `A`/`B` have one (today's path, checked first: programs without
   operator methods pay nothing).
2. Else `A`'s instance `[Symbol.op]` accepting `B`; overloads pick by argument as calls do.
3. Else `B`'s static `[Symbol.op](l: A', b: B)` with `A'` accepting `A`.
4. Else an error naming what was tried.

**Ambiguity is an error:** `A`'s instance method and `B`'s static method both accepting is
``ambiguous operator `op` `` at the use, so adding a static to `B` never changes what an existing
`a op b` calls. **Dispatch is on the left operand:** `d + b` and `b + d` may call different
methods. There is no runtime fallback (Python's `NotImplemented`): one call target per site.

### Compound assignment, `++`, `--`

`a op= b` evaluates its target once (a place, an indexer, a setter or a record field), then calls
`a[Symbol.opAssign](b)` on the place, in place, when the type has it (no new object per step:
`acc += x` on a matrix, `BigInt.addAssign`), else assigns `a = a op b`. The target must be
assignable either way, so whether a type defines `opAssign` never decides whether code compiles.
Other references to the object see an in-place change, as with Python's `+=` on a list.

**Aliasing is allowed and well-defined:** in `v += v` the method sees `other` change as it writes
`this`. The value naming the target's variable is passed as a share of the same object, which
keeps the exclusive-access rule from refusing the call.

`++a` and a statement `a++` are `a += 1` (may use `addAssign`). **A postfix `a++` whose value is
used is `<old> = a; a = <old> + 1; <old>`**, never in place (the old object would change too).

### Indexing

`a[i]` is `a[Symbol.index](i)`, `a[i] = v` is `a[Symbol.setIndex](i, v)`, `a?.[i]`
short-circuits, and `a[i] op= v` reads, applies `op=` (in place on the element with its
`opAssign`) and writes back. **A key naming a member stays property access**, as in TypeScript:
with a member `x` and `[Symbol.index](k: string)`, `a["x"]` is the member and `a[s]` the
indexer. When both methods exist, `setIndex`'s value is of the type `index` returns. A type with
`index` only is read-only. The typed arrays and `RegExpMatch` use the public keys now.

`a[i, j]` (step 8) passes several arguments; on arrays it is an error with the fix `a[i][j]`.

### Generic code

`velt/ops` exports one interface per operator (`Add<R, O = R>`, …, `Neg<O>`, `AddAssign<R>`,
`Index<K, V>`, `SetIndex<K, V>`) and `Arithmetic<T>` (`Add`, `Sub`, `Mul`, `Div`, `Neg`). A type
with an operator method implements its interface without an `implements` clause, as `compareTo`
makes a type `Comparable`; `a * b` on `T extends Mul<T>` is a `ParamMethod` call, direct after
monomorphization. Importing `velt/ops` gives the number types and `string` their operator
methods, each the built-in operator itself (`return this + other;`), so `T = number`, `f32`,
`i32` … work with exactly the operators' semantics, overflow included.

### `velt/simd`

`+`, `-`, `*` where the lane operation exists (no 8-bit `*`), `/` on floats, unary `-` on floats
and signed lanes, `& | ^ ~` on integer vectors and masks, `<<`/`>>` by a `u32` count (`>>`
arithmetic on signed lanes) and `>>>` logical on every integer vector, and `v[i]` reading a lane
(checked: a `RangeError` outside). **A scalar operand is splatted:** a literal takes the lane type,
a variable must already have it (`f32x4 * x` with `x: f64` is an error, fix `x as f32`).
`v[i] = x` is an error (fix `v = v.withLane(i, x)`): a vector is a value. Comparisons stay
methods: they give masks, and `==` gives a `bool`. The named methods stay.

### Other std types

`BigInt`: `+ - * / %` with a `BigInt` or an `i64` on either side, `<< >>` by an `i64`, unary `-`,
and in-place `+= -= *= /= %= <<= >>=` from its existing methods (it has no bitwise methods to
call). `Duration`: `+`, `-`, unary `-`, `*` and `/` by an `i64`.

## How it compiles

Sema only: each operator is rewritten into the method call it stands for, checked as that call
(`method_call_on`, `static_call`, `ParamMethod` through `iface_call`). No HIR or VIR type, no
intrinsic and no runtime function was added.

| Part | Where |
|---|---|
| The keys (`Symbol.add` … `Symbol.setIndex`, ids 15–42) | `std/lang/symbol`, `velt_syntax::OPERATOR_SYMBOLS` (one list for the parser and sema) |
| Operator ↔ method table | `velt_syntax::operators` |
| Declaration checks; implementations of `velt/ops`' interfaces | `velt_sema` `collect/operators.rs` |
| `a op b`, `-a`, `~a`; evaluation order; ambiguity | `body/expr/operators.rs` (hooked in `ops.rs` after the `any` path) |
| Scalar operands of vectors | `body/expr/operator_simd.rs` |
| `a op= b`, `++`/`--`; `opAssign` in read-then-writes | `body/expr/operator_assign.rs` (hooked in `assign.rs`, `accessor_rmw.rs`, `indexers.rs`) |
| `[]` | `body/expr/indexers.rs` (`indexes_through`; std's `INDEX` keys removed) |
| std | `std/ops` and `std/simd/ops_*.vlt` (generated: `scripts/gen-ops.py`, `scripts/gen-simd-ops.py`), `std/math/bigint_ops.vlt`, `std/time/duration.vlt` |
| Tooling | hover and go-to-definition on the operator (the call's name span is the operator); `--ts-compat` rule `operator` |

Operands are evaluated left to right. A literal, a variable or field path, or an element of one
at a pure index is passed as written (checked again as the argument, the first check dropped);
anything else goes into a temporary, the left operand first when the right one needs one.

A static and an instance method of one name in an `extend` block now have separate keys, as in
a class (`defs::static_key`), for the two forms of an operator method.

**Generic code on primitives:** the plan was a lowering rewrite of `ParamMethod` calls landing on
a number type's operator method into the built-in operation. That proved unnecessary and less
exact: sema gives `/` on integers, `+` on strings and the bitwise operators on `number` their own
intrinsics, which the method bodies (`return this op other;`) already carry, and the methods are
trivial std functions, so the debug pipeline already inlines them (`inline::std_forwarders`)
before `numrep` runs; release inlines them anyway.

## Cost

- **Run time** (instructions retired, `bench/ops/dot_generic.vlt` = `dot<number>` through
  `Arithmetic` vs `bench/ops/dot_number.vlt`, the loop on numbers, on the Linux builder under
  cachegrind): equal within 7 instructions in both pipelines, release 75.67M and debug 461.46M.
  (An earlier measurement on a busy Mac showed debug +1.7%; it does not reproduce.) A generated golden (`lang/operators_numbers.vlt`) checks 3028 operator
  results through generic code against the built-in operators.
- **Compile time:** measured with the pull request (hello, `json/parse_typed`, `check_all_std`,
  `bench/simd/kernels`).

## Diagnostics

| Code | Message |
|---|---|
| `v * v`, nothing defined | ``cannot apply binary operator `*` to type `V` `` + `help: define `[Symbol.mul](other: V)` in `V` or in an `extend V` block` |
| `2 - v` | the same, with the static form in the help |
| `T * T` unbounded | the same + `help: bound the type parameter by velt/ops' interface for it, such as T extends Mul<T>` |
| both forms apply | ``ambiguous operator `+` for `V` and `W` `` + both methods |
| `[Symbol.add](o: string)` | ``unreachable operator method: `+` with a `string` operand is always concatenation`` |
| `extend number { [Symbol.add]… }` | ``an operator method cannot be added to `f64`, whose operators are built in`` |
| `[Symbol.add](a, b)` | `` `[Symbol.add]` takes one parameter, the right operand `` |
| `static [Symbol.mul](a: V, k: f64)` | `` `V * f64` is an instance method: `[Symbol.mul](other: f64)` `` |
| `[Symbol.neg](x)`, `static [Symbol.bitNot]` | `` `[Symbol.neg]` takes no parameters ``, `` … is an instance method `` |
| `[Symbol.mulAssign](o): V` | `` `[Symbol.mulAssign]` changes `this` and returns `void` `` |
| async, generator, accessor; optional/default/rest parameter | `an operator method has plain parameters and runs synchronously` / `… takes no optional, default or rest parameter` |
| `setIndex` value vs `index` result | `` `[Symbol.setIndex]`'s value must be a `f64`, what `[Symbol.index]` returns `` |
| `m[i] = x` without `setIndex` | ``cannot assign through `[]`: `M` has `[Symbol.index]` but no `[Symbol.setIndex]` `` |
| `v * d` with `d: f64` on `f32x4` | ``a scalar operand of `f32x4` must be a `f32`, not a `f64` `` + `convert it first: x as f32` |
| `u8x16 * u8x16`, `v % v` | ``cannot apply binary operator `*` to type `u8x16` `` + `this vector type has no lane operation for this operator` |
| `v[1] = 3` | ``cannot assign to a lane of `f32x4`: a vector is a value`` + `write v = v.withLane(i, x)` |

## Steps

1. Keys and declarations.
2. Binary and unary operators on concrete types.
3. Compound assignment, `++`, `--`.
4. Indexing; std's indexers on the public keys.
5. Generics: `velt/ops`, operator methods of the number types and `string`.
6. std: `velt/simd`, `BigInt`, `Duration`.
7. Tooling and docs: hover and go-to-definition, `--ts-compat` rule `operator`, this note,
   ts-alignment.md §4, std-layout.md §3, the website's docs (docs-sync issue).
8. `a[i, j]`: an additive change of `ast::ExprKind::Index` (a list; a one-element list keeps
   today's encoding cost) in its own pull request.

## Decisions (review of #2050, 2026-11-02)

1. Symbol keys on `Symbol`.
2. A static two-operand method for a foreign left operand, with the ambiguity rule.
3. In-place `opAssign`, with the postfix and aliasing rules.
4. `==` not overloadable.
5. The named SIMD methods stay; the docs show operators first.
6. `ast::ExprKind::Index` carrying a list for `a[i, j]`, in its own pull request.

Related: #1866, #1055, #641, #1981, #1546, #924.
