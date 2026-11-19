# Operator overloading

Status: implemented. Issue: [#2050](https://github.com/velt-lang/velt/issues/2050).

## Summary

`+ - * / % **`, the bitwise operators, unary `-` and `~`, compound assignment, `[]` and `a[i, j]`
work on classes, structs, `velt/simd` vectors and type parameters. An operator is a method with a
well-known symbol key (`[Symbol.add]`), and each use resolves at compile time to one call of that
method, monomorphized and inlined: no runtime lookup and no cost over the method it calls.

## Motivation

Without operators, numeric code on user types reads as method chains:
`a.add(b.mul(0.5)).sub(c.neg())` for what the math writes `a + b * 0.5 - -c`. Matrix, vector,
complex-number, big-integer and SIMD code is the first thing a Python, Julia or Mojo programmer
writes, and the first that reads badly.

## Design

### Operator methods

Declaring an operator method opts a type in:

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
call's: own and inherited methods (virtual when overridden), `extend` blocks (with their
specificity and ambiguity rules), interface values and the bounds of a type parameter. Operands
are passed as that call passes arguments, they throw what it throws (`throws E` is allowed), and
they are evaluated left to right.

The keys are symbols, not names, because a name would turn existing methods into operators
(`Set.add(x)` must not make `set + x` compile). They are TypeScript syntax (computed keys, like
`[Symbol.iterator]`) and live on `Symbol` with Velt's other well-known symbols. `compareTo` stays
the name behind `<`: `<`, `<=`, `>` and `>=` on a `Comparable` type are `a.compareTo(b) < 0`, a
protocol with one meaning.

Classes behave as in JavaScript: `a.f(b)` leaves `b` usable and `m.at(0).x = 10` changes the
element, so an operator means exactly the call it stands for, with no new ownership rule.

### The operators

| Operator | Method | Shape |
|---|---|---|
| `a + b`, `-`, `*`, `/`, `%`, `**` | `[Symbol.add]`, `sub`, `mul`, `div`, `rem`, `pow` | `(other: R): O` |
| `a & b`, `\|`, `^`, `<<`, `>>`, `>>>` | `[Symbol.bitAnd]`, `bitOr`, `bitXor`, `shl`, `shr`, `ushr` | `(other: R): O` |
| `l op b`, only `b` of the type | `static [Symbol.<op>](l: L, b: Self): O` | first parameter not the declaring type |
| `-a`, `~a` | `[Symbol.neg]`, `[Symbol.bitNot]` | `(): O` |
| `a op= b` | `[Symbol.<op>Assign]` (`addAssign` … `ushrAssign`) | `(other: R): void`, changes `this` |
| `a[i]` | `[Symbol.index]` | `(i: K, …): V` |
| `a[i] = v` | `[Symbol.setIndex]` | `(i: K, …, v: V): void` |

**Not overloadable:** `===`/`!==` (identity); `==`/`!=` (identity on classes as in Node, content
on structs); `<` … `>=` (`Comparable`); `&&`, `||`, `??`, `!`, `in`, `instanceof`, `typeof`,
unary `+` (ToNumber). `++`/`--` are `+= 1`/`-= 1`.

**An operator method never changes what an existing program means.** It is consulted only where
the operand types have no built-in meaning for the operator: never for numbers, `string`, `bool`,
`any`/`unknown`, arrays, tuples, records or `Map` indexing. User code declaring one on such a type
is an error; std does, for generic code (below).

- **`+` with a `string` operand is always concatenation**, as in TypeScript, so a `[Symbol.add]`
  taking a `string` (or a static one whose first parameter is a `string`) is unreachable and an
  error.
- **`any` and `unknown` stay JavaScript:** an object with `[Symbol.add]` held as `any` gets
  ToPrimitive, never the method.

### Resolution of a binary operator

For `a op b` with operand types `A` and `B`:

1. The built-in meaning if `A` and `B` have one, checked first, so programs without operator
   methods pay nothing.
2. Else `A`'s instance `[Symbol.op]` accepting `B`; overloads pick by argument as calls do.
3. Else `B`'s static `[Symbol.op](l: A', b: B)` with `A'` accepting `A`.
4. Else an error naming what was tried.

**Ambiguity is an error:** when `A`'s instance method and `B`'s static method both accept, the use
is ``ambiguous operator `op` ``, so adding a static method to `B` never changes what an existing
`a op b` calls. **Dispatch is on the left operand:** `d + b` and `b + d` may call different
methods. There is no runtime fallback (Python's `NotImplemented`): one call target per site.

### Compound assignment, `++`, `--`

`a op= b` evaluates its target once (a place, an indexer, a setter or a record field), then calls
`a[Symbol.opAssign](b)` on the place, in place, when the type has it (no new object per step:
`acc += x` on a matrix), else assigns `a = a op b`. The target must be assignable either way, so
whether a type defines `opAssign` never decides whether code compiles. Other references to the
object see an in-place change, as with Python's `+=` on a list.

**Aliasing is allowed and well defined:** in `v += v` the method sees `other` change as it writes
`this`. The value naming the target's variable is passed as a share of the same object, which
keeps the exclusive-access rule from refusing the call.

`++a` and a statement `a++` are `a += 1` (and may use `addAssign`). **A postfix `a++` whose value
is used is `<old> = a; a = <old> + 1; <old>`**, never in place, since the old object would change
too.

### Indexing

`a[i]` is `a[Symbol.index](i)`, `a[i] = v` is `a[Symbol.setIndex](i, v)`, `a?.[i]`
short-circuits, and `a[i] op= v` reads, applies `op=` (in place on the element with its
`opAssign`) and writes back. **A key naming a member stays property access**, as in TypeScript:
with a member `x` and `[Symbol.index](k: string)`, `a["x"]` is the member and `a[s]` the indexer.
When both methods exist, `setIndex`'s value is of the type `index` returns. A type with `index`
only is read-only. std's typed arrays and `RegExpMatch` use the same public keys.

**`m[i, j]` passes several keys:** `m[Symbol.index](i, j)`, and `m[Symbol.setIndex](i, j, v)` as
a target, with `op=`, `++`, `--` and `?.[i, j]` as for one key. The object is evaluated first,
then each key once, left to right, then the value; `m?.[f(), g()]` evaluates no key when `m` is
null or undefined, as JavaScript short-circuits the chain, and `m[i, j] op= m[i, j]` is well
defined, as `v += v` is. Several keys always call the indexer (the member-key rule needs one key).
`[Symbol.setIndex]` takes `[Symbol.index]`'s keys, of the same types position by position, and
then the value. Every other type (arrays, tuples, strings, `Map`s, records, typed arrays, `any`,
an indexer taking one key) is an error with the fix `a[i][j]`. `velt/ops` has no interfaces with
several keys; a plain interface declaring the indexer serves generic code.

### Generic code

`velt/ops` exports one interface per operator (`Add<R, O = R>`, …, `Neg<O>`, `AddAssign<R>`,
`Index<K, V>`, `SetIndex<K, V>`) and `Arithmetic<T>` (`Add`, `Sub`, `Mul`, `Div`, `Neg`). A type
with an operator method implements its interface without an `implements` clause, as `compareTo`
makes a type `Comparable`; `a * b` on `T extends Mul<T>` is a call through the bound, direct after
monomorphization. Importing `velt/ops` gives the number types and `string` their operator
methods, each the built-in operator itself (`return this + other;`), so `T = number`, `f32`,
`i32` … work with exactly the operators' semantics, overflow included.

### `velt/simd`

`+`, `-`, `*` where the lane operation exists (no 8-bit `*`), `/` on floats, unary `-` on floats
and signed lanes, `& | ^ ~` on integer vectors and masks, `<<`/`>>` by a `u32` count (`>>`
arithmetic on signed lanes) and `>>>` logical on every integer vector, and `v[i]` reading a lane
(checked: a `RangeError` outside). **A scalar operand is splatted:** a literal takes the lane
type, a variable must already have it (`f32x4 * x` with `x: f64` is an error; the fix is
`x as f32`). `v[i] = x` is an error (the fix is `v = v.withLane(i, x)`): a vector is a value.
Comparisons stay methods, because they give masks while `==` gives a `bool`. The named methods
(`a.add(b)`) remain.

### Other std types

- `BigInt`: `+ - * / %` and `& | ^` on two BigInts, `**` and `<< >>` by an `i64`, unary `-`
  and `~`, and in-place `+= -= *= /= %= <<= >>=`. Any other number operand is an error, as in
  JavaScript
  (`TypeError: Cannot mix BigInt and other types`) and TypeScript (TS2365); the note names
  `new BigInt(k)` or the named method that takes an `i64` (`b.addInt(3)`). Accepting
  `BigInt | i64` instead would cost every operation a test of which it got.
- `Duration`: `+`, `-`, unary `-`, and `*` and `/` by an `i64` (with `k * d` through a static
  method).

## Compilation

The front end alone implements operators: each use is rewritten into the method call it stands
for and checked as that call. No HIR or VIR type, intrinsic or runtime function exists for them.

| Part | Where |
|---|---|
| The keys (`Symbol.add` … `Symbol.setIndex`, ids 15–42) | `std/lang/symbol/index.velt`; `velt_syntax::OPERATOR_SYMBOLS`, one list for the parser and sema |
| Operator ↔ method table | `crates/velt_syntax/src/operators.rs` |
| Declaration checks; implementations of `velt/ops`' interfaces | `crates/velt_sema/src/collect/operators.rs` |
| `a op b`, `-a`, `~a`; evaluation order; ambiguity | `crates/velt_sema/src/body/expr/operators.rs`, hooked in `ops.rs` after the `any` path |
| Scalar operands of vectors | `crates/velt_sema/src/body/expr/operator_simd.rs` |
| `a op= b`, `++`/`--`; `opAssign` in read-then-writes | `crates/velt_sema/src/body/expr/operator_assign.rs`, hooked in `assign.rs`, `accessor_rmw.rs`, `indexers.rs` |
| `[]` | `crates/velt_sema/src/body/expr/indexers.rs` |
| `[i, j]` | `ast::ExprKind::Index`'s `more` (the keys after the first), `parser/postfix.rs`, `crates/velt_sema/src/body/expr/index_keys.rs`, `same_keys` in `collect/operators.rs` |
| `<` on `Comparable` | `crates/velt_sema/src/body/expr/ordering.rs` |
| std | `std/ops`, `std/simd/ops_*.velt` (generated by `scripts/gen-ops.py` and `scripts/gen-simd-ops.py`), `std/math/bigint_ops.velt`, `std/time/duration.velt` |
| Tooling | hover and go-to-definition on the operator (the call's name span is the operator); `--ts-compat` rule `operator` |

Operands are evaluated left to right. A literal, a variable or field path, or an element of one
at a pure index is passed as written; anything else goes into a temporary, the left operand first
when the right one needs one.

A static and an instance method of one name in an `extend` block have separate keys
(`defs::static_key`), as in a class, for the two forms of an operator method.

**Generic code on primitives** needs no special lowering. Sema gives `/` on integers, `+` on
strings and the bitwise operators on `number` their own intrinsics, which the std method bodies
(`return this op other;`) carry; the methods are trivial std functions, so the debug pipeline
inlines them (`std_forwarders` in `crates/velt_opt/src/inline/mod.rs`) before `numrep` runs, and
release inlines them anyway.

## Runtime cost

None over the method called. `dot<number>` through `Arithmetic`
(`bench/ops/dot_generic.velt`) and the same loop on numbers (`bench/ops/dot_number.velt`) retire
the same instructions within 7 in both pipelines (release 75.67 M, debug 461.46 M; Linux,
cachegrind). A generated golden (`tests/golden/lang/operators_numbers.velt`) checks 3028 operator
results through generic code against the built-in operators.

## Diagnostics

| Code | Message |
|---|---|
| `v * v`, nothing defined | ``cannot apply binary operator `*` to type `V` ``, help ``define `[Symbol.mul](other: V)` in `V` or in an `extend V` block`` |
| `2 - v` | the same, with the static form in the help |
| `T * T` unbounded | the same, help ``bound the type parameter by `velt/ops`' interface for it, such as `T extends Mul<T>` `` |
| both forms apply | ``ambiguous operator `+` for `V` and `W` ``, with both methods |
| `[Symbol.add](o: string)` | ``unreachable operator method: `+` with a `string` operand is always concatenation`` |
| `extend number { [Symbol.add]… }` | ``an operator method cannot be added to `f64`, whose operators are built in`` |
| `[Symbol.add](a, b)` | `` `[Symbol.add]` takes one parameter, the right operand `` |
| `static [Symbol.mul](a: V, k: f64)` | `` `V * f64` is an instance method: `[Symbol.mul](other: f64)` `` |
| `[Symbol.neg](x)`, `static [Symbol.bitNot]` | `` `[Symbol.neg]` takes no parameters ``, `` `[Symbol.bitNot]` is an instance method: only the binary operators have a static form `` |
| `[Symbol.mulAssign](o): V` | `` `[Symbol.mulAssign]` changes `this` and returns `void` `` |
| async, generator, accessor | ``an operator method has plain parameters and runs synchronously: `…` cannot be async, a generator or an accessor`` |
| optional, default or rest parameter | ``an operator method has plain parameters: `…` takes no optional, default or rest parameter`` |
| `setIndex` value vs `index` result | `` `[Symbol.setIndex]`'s value must be a `f64`, what `[Symbol.index]` returns `` |
| `m[i] = x` without `setIndex` | ``cannot assign through `[]`: `M` has `[Symbol.index]` but no `[Symbol.setIndex]` `` |
| `v * d` with `d: f64` on `f32x4` | ``a scalar operand of `f32x4` must be a `f32`, not a `f64` ``, note ``convert it first: `x as f32` `` |
| `u8x16 * u8x16`, `v % v` | ``cannot apply binary operator `*` to type `u8x16` ``, note `this vector type has no lane operation for this operator` |
| `v[1] = 3` | ``cannot assign to a lane of `f32x4`: a vector is a value``, note ``write `v = v.withLane(i, x)` `` |

## Compatibility

Arithmetic, bitwise and unary operators on objects, `obj[i]` without an index signature, and
`a[i, j]` are type errors in TypeScript (in TypeScript `a[i, j]` is the comma operator, which
reads `a[j]`), so giving them a meaning changes no valid TypeScript program. The computed-key
declarations (`[Symbol.add](…)`) are TypeScript syntax. `--ts-compat` reports operator uses under
the rule `operator`.

## Alternatives considered

**Methods by name** (Kotlin's `plus`). A name turns existing methods into operators: `Set.add(x)`
would make `set + x` compile. Symbol keys make opting in explicit.

**A runtime fallback** (Python's `NotImplemented`, trying the right operand when the left
declines). It costs a dynamic test per use and makes the call target depend on values; Velt gives
each site one target, with the static form and the ambiguity rule covering a foreign left operand.

**Overloadable `==`.** `==` is identity on classes, as in Node, and content on structs; making it
user-defined would let it disagree with `Set` and `Map` membership.

**Comparison operators returning masks on vectors.** `==` must give a `bool` everywhere, so
vector comparisons stay methods that return masks.

## Limitations

- `===`, `==`, the comparison operators, the logical operators and unary `+` cannot be
  overloaded.
- An operator method cannot be declared on a type whose operators are built in (numbers,
  `string`, `bool`, arrays, tuples, records), except by std.
- A vector lane cannot be assigned through `v[i] = x`.
