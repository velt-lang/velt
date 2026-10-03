# Design: utility types and `keyof` on type parameters

Status: proposed (issue #350). Nothing here is implemented. It builds on the utility types for
concrete object types (`shared-models.md`, issue #326), which this note calls *the concrete
rules*, and changes nothing about them.

## Problem

TypeScript's utility types are mostly used in generic code:

```ts ignore
function update<T>(x: T, patch: Partial<T>): T {
  return { ...x, ...patch };
}

function pluck<T, K extends keyof T>(xs: T[], k: K): T[K][] {
  return xs.map((x) => x[k]);
}

class Form<T> {
  draft: Partial<T> = {};
}
```

Velt resolves every type eagerly. Inside `update`, `T` is `TyKind::Param(0)`: it has no fields
and only its bounds are known. Generic bodies are checked once, with `T` opaque
(`body/driver.rs`). Substitution is structural (`Types::subst`, `velt_vir`'s `Cx::subst`), and
monomorphization happens later, in `velt_vir`, which has no diagnostics. So the concrete rules
can't reduce `Partial<T>`, and they report
`` `Partial` needs a concrete object type; `T` is a type parameter ``.

## What the concrete rules provide

This design depends on three things from shared-models.md. If that note settles them
differently, the affected sections below change with it.

1. **Reduction.** `reduce(op, args)` turns `Partial`, `Required`, `Pick` or `Omit` of a concrete
   object type into an anonymous object type, with its fields in source order, or reports an
   error. `Readonly<X>` keeps `X`'s representation, so a value of type `X` is a `Readonly<X>`
   without a copy. Readonly is a static restriction.
2. **Structural field-only bounds.** `T extends { name: string }` means that `T` is an object type
   with a field `name` of exactly `string`. Velt has no depth or width subtyping between object
   types, so the field's type is exact.
3. **Optional fields** stay `F | null`. This is today's rule (`ast.rs`).

## Proposal

### Representation: a stuck operator

`TyKind` gets one variant:

```rust
/// A type operator applied to arguments that mention a type parameter, so it can't be reduced
/// yet. Never interned with arguments it could reduce.
Op(TyOp, Vec<TyId>),

pub enum TyOp { Partial, Required, Readonly, Pick, Omit, KeyOf, Index }
```

`Index` is `T[K]`. `Pick` and `Omit` take `(T, K)`, `KeyOf` takes `(T)`, and `Index` takes
`(T, K)`.

- **The invariant** is that an `Op` is *stuck*: its operand's head is a `Param`, or another stuck
  `Op`. Everything else reduces at once, generic or not, because anonymous object types are
  already generic over their field types (`anon.rs`). For example, `Partial<{ a: U }>` is
  `{ a: U | null }` inside `f<U>`. `Partial<U[]>` is an error where it is written, because no
  `U` can make an array an object type.
- **Resolution.** `resolve.rs` builds operator types through one constructor,
  `Ctx::op(op, args, span)`. It applies the concrete rules when it can, and otherwise interns the
  stuck form. `keyof T` and `T[K]` need parser support (below). The others are names, resolved
  like the `Array` and `Promise` builtins. A user item with the same name shadows them.
- **Substitution stays structural.** `Types::map`, `children` and `collect_params` treat `Op` like
  `Adt` (they recurse into the arguments), so `subst` may produce an `Op` that could now reduce.
  The callers that inspect the result normalize it with `Ctx::normalize(t)`, which re-applies
  `Ctx::op` bottom-up: the signature at a call site after slots are solved, field lookup, and
  `coerce`. This keeps `Types` independent of the definition table. Reduction can create
  anonymous defs, so `Types` alone can't do it.
- **Interning.** Equal stuck forms have equal ids, as for any other type, so `Partial<T>` in a
  parameter and in the return type are the same type. Two different operators over the same `T`
  are different types, even where they would reduce to the same object type for every `T`
  (`Pick<T, keyof T>` and `Required<Required<T>>`, for example). They compare equal only after
  reduction.

### Member access on a stuck form

Inside the generic body, the fields of `T` are the fields of its structural bounds, from the
concrete rules' point 2. Write `B(T)` for them. A deferred value is always a plain anonymous
object once it is reduced, never a class, so its fields are read *and written* by name. The
exception is `Readonly`.

| Type of `p` | Fields `p.f` can name | Type of `p.f` | `p.f = v` |
|---|---|---|---|
| `Partial<T>` | `B(T)` | `F \| null` | yes |
| `Required<T>` | `B(T)` | `F` without `\| null` | yes |
| `Readonly<T>` | `B(T)` | `F` | no |
| `Pick<T, K>` | `B(T)` ∩ the keys `K` is known to contain | `F` | yes |
| `Omit<T, K>`, `K` a literal union | `B(T)` minus `K` | `F` | yes |
| `Omit<T, K>`, `K` a parameter | none | — | — |
| `T[K]` | the fields of the bound type, if `K` names exactly one field of `B(T)` (`T["id"]`); otherwise none | — | — |

`F` is the field's type in `B(T)`. "Known to contain" means that `K` is a literal union, or a
parameter with a bound that is a literal union. A field outside the table is an error that names
the operator and the bound:

```text
error: `Partial<T>` has no known field `email`
  = note: inside `update`, `T` has the fields of `{ name: string }`
  = help: add the field to the bound: `T extends { name: string; email: string }`
```

`T` itself, with structural bounds, is read and written by name the same way (the concrete
rules' point 2). Interface bounds keep today's getter calls.

**HIR.** There is one new expression, `ExprKind::FieldByName { base, name }`, for a field of a
value whose type is a stuck form or a parameter with structural bounds. `velt_vir` resolves the
name to a field index after substitution, once the type is a concrete anonymous `Adt`. It is the
same load as a written field access.

### Object literals and spreads at a deferred type

`{ ...x, ...patch }`, typed as `T`, can't be desugared into a struct literal as `spread.rs` does
today, because the field list isn't known. Sema checks it symbolically and emits
`ExprKind::DeferredObject { ty, parts }`. Here `parts` are spreads and named properties in
source order. `velt_vir` expands it into the same merged struct `spread.rs` would build for the
concrete type. Spreads keep their present rules (a later key wins, and the key keeps its first
position).

**Coverage.** A literal of a deferred or parameter type must set every field. Each part covers
some fields:

| Part | Covers |
|---|---|
| `...x` with `x: T`, `Required<T>` or `Readonly<T>` | `keyof T` |
| `...p` with `p: Pick<T, K>` | `K` |
| `...p` with `p: Omit<T, K>` | `keyof T` minus `K` |
| `...p` with `p: Partial<T>` | nothing; it may override fields |
| `name: e` | `name`, which must be in `B(T)`, with `e: F` |

The literal is accepted when its parts cover `keyof T`. If some fields might be missing, the
error says which, for example "a `Partial<T>` may lack every field".

**Overriding with a `Partial` spread.** TypeScript copies a key that is present and skips one
that is missing. Velt doesn't distinguish a missing field from `null`: optional fields are
`F | null`. So spreading a `Partial<T>` (or any `F | null` field into an `F` target) copies the
field only when it is not `null`. Each such field costs one test. This is what
`{ ...x, ...patch }` means in TypeScript code that never writes `undefined` explicitly. Velt has
no `undefined`. The same rule applies to concrete `Partial<X>` spreads, so that the concrete and
generic versions agree.

**No implicit conversions.** A `T` is not a `Partial<T>` or a `Pick<T, K>`: they have different
layouts. A conversion would have to build a new object, but in TypeScript it is the same object,
and with shared references (semantics stage 2) the difference shows when the result is mutated.
Use `{ ...x }`, which makes a copy in TypeScript too. `T` to `Readonly<T>` is allowed, because
it is the same value (the concrete rules' point 1).

### `keyof T` and `T[K]`

**Syntax.** `keyof` becomes a contextual keyword in type position: a type prefix that binds
tighter than `|`. Indexed access `T[K]` becomes a postfix in type position. `T[]` stays an array
type, so an empty `[]` is an array and `[K]` is indexed access. `TypeExprKind` gains
`KeyOf(Box<TypeExpr>)` and `Index(Box<TypeExpr>, Box<TypeExpr>)`. That is a change to the
`ast.rs` contract.

**Meaning.**

- `keyof X`, for a concrete object type `X`, is the union of its field names as string literal
  types, such as `"id" | "name"`.
- `keyof Record<K, V>` is `K`.
- `keyof` of anything else is an error. TypeScript's `number` and `symbol` keys don't exist in
  Velt.
- `X[K]` is the union of the types of the fields named by `K`. `K` must be a key of `X`.

**Key bounds.** `K extends keyof T` is a new kind of bound, next to interface bounds and
structural bounds. In the body, `x[k]` with `x: T` and `k: K` has type `T[K]`. That type is
opaque: it can be moved, stored, returned and passed to generics. It is only known more precisely
where the table above says so. `xs[k]` on an array is unchanged.

**Inference.** A parameter with a key bound is inferred *without widening*. In
`pluck(users, "name")`, `K` is `"name"`, not `string`, as in TypeScript. This is the same rule
literal arguments already follow against literal-typed parameters.

**Run time.**

- A literal type is zero-sized (`velt_vir/src/lower/types.rs`), so `k: K` with `K = "name"`
  costs nothing. `x[k]` becomes the same load as `x.name`.
- With `K = "a" | "b"`, `k` is a union tag. `x[k]` becomes a switch on that tag, which loads the
  field and wraps it as a member of the union `T[K]`. That is one branch, and no hashing or string
  comparison.
- **Writes** `x[k] = v` are allowed. When `K` reduces to several keys whose fields have different
  types, that instantiation is an error, because `v` couldn't be stored without a run-time check:

  ```text
  error: `setField` writes `T[K]` through `k`, but with `K = "id" | "name"` the fields have different types
     --> src/main.vlt:12:3
      = note: `id: i64`, `name: string`
  ```

`ExprKind::KeyIndex { base, key }` is the HIR form. `velt_vir` lowers it to a load or a switch,
as above.

### Errors at instantiation

The operators impose requirements on their operands:

- `Partial<T>`: `T` is an object type.
- `Pick<T, K>`: `K` names fields of `T`.
- `K extends keyof T`: the same.
- `T[K]`: the same.

The requirements are implicit. TypeScript doesn't ask for `T extends object`, so neither does
Velt. They are checked exactly as generic `Record` keys are today (`record_keys.rs`), and that
pass is generalized to carry any requirement:

1. Each generic def records the stuck forms its types mention. That covers the signature, locals,
   expression types and closure types, each with the span where it was written.
2. A fixpoint over call sites substitutes the caller's type arguments into each callee's needs. A
   need that reduces is recorded (see "What `velt_vir` sees"). A need that fails becomes an error
   at the concrete call site, with the note ``required because `update` uses `Partial<T>` ``
   pointing at the use. A need that is still stuck propagates to the caller.
3. Instantiations through interfaces and base classes come from `dispatch.rs`, as for the JSON
   and `Record` checks. A generic class's field types are checked where the class type is
   resolved with concrete arguments (`def_type`, as for `Record`).

```text
error: `i64` is not an object type
   --> src/main.vlt:20:10
    |
 20 |   update(5, {});
    |          ^ `T = i64`
   --> src/lib.vlt:3:33
    |
  3 | function update<T>(x: T, patch: Partial<T>): T {
    |                                 ---------- required because `update` uses `Partial<T>`
```

The bound `K extends keyof T` is checked at each call where `T` and `K` are known, through the
same pass when a caller is itself generic. The message lists the keys:

```text
error: `"emial"` is not a key of `User`
  = note: the keys are "id" | "name" | "email"
```

### What `velt_vir` sees

`velt_vir` can intern types but can't create definitions, so it can't reduce `Partial<User>`
itself. Sema gives it a table, `hir::Program::reductions: HashMap<TyId, TyId>`.

- Keys are operator types over a definition applied to its own parameters, such as
  `Partial<Adt(d, [P0, …, Pn])>`. Values are the reduced type in terms of the same parameters.
  Anonymous object types are generic over their field types, so one entry serves every type
  argument list of `d`, and the table grows with the number of shapes, not of instantiations.
- In `Cx::subst`, an `Op` is handled in three steps. Substitute its arguments, which gives a head
  `Adt(d, args)`. Look up the key with `d`'s identity arguments. Substitute `args` into the value.
  A missing entry is an `ICE:`, since the sema pass reached every instantiation.
- `KeyOf` values are literal unions, so they have no parameters. `Index` values are field types,
  or a union of them.

After this step, VIR has no stuck forms. Layouts, drop glue, printing and JSON see ordinary
anonymous objects, unions and literals.

## Cost

- **Run time:** none. Every operator is reduced before monomorphization. A field read on a
  `Partial<T>` is one load, as on a written object type. `x[k]` is a load, or one switch when
  `K` is a union. The only added work is what the source asks for: a null test per field when a
  `Partial` spread overrides, and copies for `{ ...x }`.
- **Compile time:** one more `TyKind` variant, normalization at call sites, and needs propagation
  in the existing fixpoint pass. Its size is bounded by the generic defs that mention a stuck
  form.

## Contract changes

These need maintainer sign-off (CLAUDE.md rule 1):

- `hir/mod.rs`: the `TyKind::Op` variant and `TyOp`; `ExprKind::FieldByName`,
  `DeferredObject` and `KeyIndex`; and `Program::reductions`.
- `ast.rs`: `TypeExprKind::KeyOf` and `TypeExprKind::Index`, and key bounds in generic parameter
  lists. The bound syntax `K extends keyof T` already parses as a type once `keyof` exists.
- `velt_vir`: `Cx::subst` handles `Op` through the table.

## Diagnostics

- `` `Partial` needs a concrete object type; `T` is a type parameter ``: removed. Its cases become
  the instantiation errors above.
- `` `Partial<T>` has no known field `f` ``, with the bound as a note and the fix (extend the bound).
- `` a `Partial<T>` may lack every field `` on a literal typed `T`, listing the uncovered fields,
  with the help ``spread a value of type `T` first``.
- `` `Partial<U[]>`: `U[]` is not an object type `` where it is written.
- `` `i64` is not an object type ``, or `` `"x"` is not a key of `User` ``, at a concrete call
  site, with ``required because `f` uses `Partial<T>` ``.
- The `T[K]` write error above.
- ``cannot infer type parameter `T` of `f` ``, when `T` appears only under an operator, gets the
  help `` `T` appears only inside `Partial<T>`; write `f<User>(…)` ``. Operators are not inference
  sites: `Partial<A>` and `Partial<B>` can be the same type for different `A` and `B`.
- `` `T` is not a `Partial<T>` `` on a missing implicit conversion, with the help `copy it: { ...x }`.

## Not proposed

- **Mapped and conditional types** (`{ [P in keyof T]: … }`, `T extends U ? X : Y`). They are
  general type-level programs. The five operators and `keyof` cover the common uses without one.
  `Op` can take more operators later.
- **Inferring through operators** (TypeScript's reverse mapping for homomorphic mapped types).
- **Re-checking generic bodies per instantiation**, C++ template style. Errors would point into
  library code, compile time would grow with every instantiation, and it would break the rule that
  a generic body is checked once.
- **Classes as operands.** Reducing `Partial<Class>` follows the concrete rules. If they reject
  classes, a class type argument is an instantiation error here too.

## Open questions

1. **Override semantics of `Partial` spreads.** The proposal is to skip `null` fields. The
   alternative, copying `null`, is closer to TypeScript's treatment of an explicit `undefined`.
   But it would make `{ ...x, ...patch }` erase fields, which is almost never what the code means.
2. **`Omit<T, K>` with a parameter `K`.** No field can be named. Is it worth supporting before a
   use case shows up, or should it be an error ("needs a literal key type")?
3. **Writes through `T[K]` with union keys.** The proposal is an instantiation error. The
   alternative is to reject `x[k] = v` in generic code altogether, which is simpler but
   unnecessary for the common single-key calls.
