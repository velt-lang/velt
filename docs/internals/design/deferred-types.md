# Design: utility types and `keyof` on type parameters

Status: proposed (issue #350). Nothing here is implemented. The utility types for concrete object
types are planned separately (issue #326, `shared-models.md`), but that note is not written yet.
This note therefore specifies the concrete rules too ("Meaning"), so that the generic rules have
something to reduce to. If #326 settles them differently, the two notes must be reconciled. The
TypeScript behaviour cited here was checked with `tsc` 6.0.2 `--strict`.

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

Velt resolves every type eagerly. Inside `update`, `T` is `TyKind::Param(0)`: it has no fields,
and only its bounds are known. Generic bodies are checked once, with `T` opaque
(`body/driver.rs`). Substitution is structural (`Types::subst`, and `Cx::subst` in `velt_vir`).
Monomorphization happens later, in `velt_vir`, which has no diagnostics.

## Prerequisites

Three changes have to land first. Each one is useful without this note.

**P1. Canonical instantiation.** Substituting into a generic anonymous object type or union
doesn't produce the type that would have been written directly. This is a bug today:

```ts ignore
function wrap<U>(x: U): { a: U } { return { a: x }; }
const o: { a: string } = wrap<string>("hi");
// error: mismatched types: expected { a: string }, found { a: string }
```

`U | null` with `U = string | null` gives `string | null | null`, and a generic union `P0 | P1`
with `P0 = P1 = "a"` keeps a duplicate member. Substitution in sema must re-canonicalize these
forms: re-intern anonymous shapes from their substituted fields, flatten and deduplicate
unions, and collapse nested `null`. `velt_vir`'s `Cx::subst` must do the same. Every reduction
below relies on this.

**P2. `?:` is a flag, not a type.** Today `name?: T` is parsed as `name: T | null`, and the
flag only keeps the spelling (`ast.rs`, `anon.rs`). Instead, `FieldInfo` keeps the declared type
`T` plus `optional: true`, and the flag becomes part of an anonymous type's identity. The field
still holds `T | null`, with the same layout and the same reads. This is what lets `Required`
remove exactly what `?` added (TypeScript keeps an explicit `| null`), and lets spreads tell an
absent key from a `null` value (see "Spreads").

`{ a?: T }` and `{ a: T | null }` become two types with the same layout. Converting between
them at the top level of a value is free (a coercion in `coerce.rs`), so existing code keeps
compiling. They are not interchangeable inside other types (`{ a?: T }[]`), like any two
different object types.

**P3. Structural field-only bounds.** `T extends { name: string; id?: i64 }` means that `T` is
an anonymous object type with at least these fields, each with exactly this type and
optionality. Velt has no depth subtyping between object types. Interface bounds stay as they
are. Write `B(T)` for the fields that `T`'s bounds declare. Inside the body, `x.name` with `x: T`
reads and writes the field by name (`ExprKind::FieldByName`, below). At a call site, the bound is
checked like an interface bound. A class doesn't satisfy a structural bound: its fields can be
private or getters, and it is a reference with its own identity.

## Meaning

The operators apply to **object types**: anonymous object types and aliases of them. A class,
`Record`, an array or a primitive is not an object type.

| Type | Result |
|---|---|
| `Partial<X>` | every field of `X`, with `optional` set |
| `Required<X>` | every field of `X`, with `optional` cleared (`a?: T` becomes `a: T`; `a: T \| null` stays) |
| `Readonly<X>` | `X` seen through a read-only view (below) |
| `Pick<X, K>` | the fields of `X` named by `K`, in `X`'s order; every member of `K` must be a field |
| `Omit<X, K>` | the fields of `X` not named by `K`; members of `K` that are not fields are ignored, as in TypeScript |
| `keyof X` | the union of `X`'s field names as string literal types |
| `X[K]` | the union of the types of the fields named by `K` (an optional field contributes `T \| null`) |

- **Unions.** `Partial`, `Required` and `Readonly` distribute over unions, as TypeScript's
  homomorphic mapped types do: `Partial<A | B>` is `Partial<A> | Partial<B>`, and
  `Partial<X | null>` is `Partial<X> | null`. `Pick`, `Omit` and `keyof` of a union are errors
  that suggest writing the union of the picks.
- **Keys.** A key type `K` is a string literal or a union of them.
- **Precedence.** As in TypeScript, postfix `[]` and `[K]` bind tighter than `keyof`: `keyof X[]`
  is the keys of an array, which is an error.
- **Combined forms.** `keyof Partial<X>`, `keyof Required<X>` and `keyof Readonly<X>` are
  `keyof X`. `keyof Pick<X, K>` is `K`. `Partial<X>[K]` is `X[K] | null`.
- **`Readonly<X>`** is a view, not a new shape. Writes through it are errors. `X` converts to
  `Readonly<X>` and back for free, because they are the same value. TypeScript allows both
  directions, so `readonly` is a shallow lint there and here. It is the one operator that stays
  in the type after reduction: `velt_vir` lays it out as `X`.

## Representation: a stuck operator

`TyKind` gets one variant:

```rust
/// A type operator that can't be reduced yet, because an argument it inspects is a type
/// parameter or another stuck operator. Also the permanent form of `Readonly`.
Op(TyOp, Vec<TyId>),

pub enum TyOp { Partial, Required, Readonly, Pick, Omit, KeyOf, Index }
```

- **Stuck.** An operator is stuck when an argument it inspects has a `Param` or a stuck `Op` at
  its head. For `Partial<T>`, `Pick<T, K>` and `T[K]` that is the object operand. For
  `Pick<User, K>`, `Omit<User, K>` and `User[K]` it is the key operand, as in the common
  `get<K extends keyof User>(u: User, k: K): User[K]`. Everything else reduces where it is
  written, generic or not. `Partial<{ a: U }>` is `{ a?: U }` inside `f<U>`, and that holds for
  every `U` (P1, P2). `Partial<U[]>` is an error where it is written, because no `U` makes an
  array an object type.
- **Resolution.** `resolve.rs` builds operators through one constructor, `Ctx::op(op, args)`.
  It reduces when it can, and otherwise interns the stuck form. `keyof` and `T[K]` need parser
  support. The other operators are names, resolved like the builtins `Array` and `Promise`, and a
  user item with the same name shadows them.
- **Every substitution normalizes.** Substitution in sema goes through `Ctx::subst`, which
  re-applies `Ctx::op` bottom-up after substituting. `Types::subst` becomes private to it. So the
  invariant "no reducible `Op` is ever interned" holds everywhere, including the sites that
  substitute today:
  - argument and default types (`args.rs`);
  - impl-signature matching (`collect/impls.rs`);
  - interface inheritance (`iface_extends.rs`);
  - `dispatch.rs`;
  - the record-key, JSON and `void` instantiation passes.

  A form over a callee's still-unknown slot becomes `Error` with the slot (`subst_known`). It
  reduces to `Error` without a second diagnostic.
- **Simplification.** These laws hold for every `T` under P2, and normalization applies them:
  - `Partial<Partial<T>>`, `Partial<Required<T>>` and `Partial<Readonly<T>>` are `Partial<T>`;
  - likewise with `Required` outside;
  - `Readonly<Readonly<T>>` is `Readonly<T>`.

  They keep stuck forms small, and they make polymorphic recursion through an operator visible
  (see "Errors at instantiation").
- **Equality.** Equal stuck forms have equal ids. `Pick<T, keyof T>` and `T` are different types
  in the body, although they reduce to the same type. TypeScript relates them, but Velt doesn't
  need to.

## Member access in the generic body

A value of a stuck `Partial`, `Required`, `Pick` or `Omit` type is always an anonymous object
once it is reduced, so its fields are read *and written* by name. A `Readonly` value is read only.
In the table, `F` and `opt` are the field's type and optionality in `B(T)`:

| Type of `p` | Fields `p.f` can name | Type of `p.f` | `p.f = v` |
|---|---|---|---|
| `T` | `B(T)` | `F`, or `F \| null` if `opt` | yes |
| `Partial<T>` | `B(T)` | `F \| null` | yes |
| `Required<T>` | `B(T)` | `F` | yes |
| `Readonly<T>` | `B(T)` | as for `T` | no |
| `Pick<T, K>`, `K` a literal union | `B(T)` ∩ `K` | as for `T` | yes |
| `Omit<T, K>`, `K` a literal union | `B(T)` minus `K` | as for `T` | yes |
| `Pick<T, K>` or `Omit<T, K>`, `K` a parameter | none | — | — |

`Pick<T, K>` names no field when `K` is a parameter, even with `K extends "a" | "b"`: a bound is
only an upper bound, and `K = "a"` leaves `b` out. TypeScript rejects `x.a` there too (TS2339),
and also for `Omit<T, K>`. `p[k]` with `k: K` is the way to read or write those fields.

**HIR.** `ExprKind::FieldByName { base, name }` reads or writes a field of a value whose type is
a parameter with structural bounds, or a stuck form. `velt_vir` resolves the name to a field
index after substitution. It compiles to the same load or store as a written field.

## Object literals and spreads at a deferred type

`{ ...x, ...patch }` typed as `T` can't be desugared into a struct literal the way `spread.rs`
does today, because the field list isn't known. Sema checks it symbolically and emits
`ExprKind::DeferredObject { ty, parts }`, where `parts` are the spreads and named properties in
source order. `velt_vir` expands it after substitution into the merged struct that `spread.rs`
would build for the concrete type. A later key wins, and a key keeps its first position. Fields a
spread provides that the target doesn't have are ignored, as today.

**Coverage.** What a literal must provide depends on its target type:

| Target | Must cover |
|---|---|
| `T`, `Readonly<T>` | `keyof T`; optional fields may be left out |
| `Required<T>` | `keyof T` |
| `Pick<T, K>` | `K` |
| `Omit<T, K>` | `keyof T` minus `K` |
| `Partial<T>` | nothing, so `draft: Partial<T> = {}` is fine |

These parts cover fields:

| Part | Covers |
|---|---|
| `...x` with `x: T`, `Required<T>` or `Readonly<T>` | `keyof T` |
| `...p` with `p: Pick<T, K>` | `K` |
| `...p` with `p: Omit<T, K>` | `keyof T` minus `K` |
| `...p` with `p: Partial<T>` | nothing; it may override fields |
| `name: e` | `name`, which must be in `B(T)`, with `e` of the field's type |

`{ ...p }` with `p: Partial<T>`, as a `T`, is rejected. TypeScript rejects it too (TS2322).

**Spreads skip absent keys.** In TypeScript, `{ ...x, ...patch }` copies the keys that are
present in `patch` and nothing else. A present key is copied even when its value is `null` or
`undefined`:

```text
update(u, {})                   → {"id":1,"name":"a"}
update(u, { name: "b" })        → {"id":1,"name":"b"}
{ ...n, ...{ a: null } }        → {"a":null}
```

Velt has no `undefined`. Under P2, an absent optional key is `null`. So spreading an **optional**
(`?:`) field copies it only when it is not `null`, and a non-optional field is always copied,
`null` included. This rule applies to every spread, concrete or generic, and it is decided by
the source field's `optional` flag after reduction, so both agree. It gives TypeScript's result
in every case but one: a patch can't set an optional field to `null`, because `{ a: null }` and
`{}` are the same value of type `Partial<X>`. `Partial` exists to describe absent keys, so this
case is rare, and the reference documents it with the fix (`{ ...update(u, p), a: null }`).

Under the rule, a field read from an optional source becomes a branch:
`if (p.f != null) { out.f = p.f; /* x.f dropped */ } else { out.f = x.f }`. Both sources are
consumed by the literal, as they are today. The branch decides which of the two values is
dropped. That is local to the generated code and needs no drop flags.

**No implicit conversions.** A `T` doesn't convert to `Partial<T>`, `Pick<T, K>` or
`Omit<T, K>`, because they have different layouts. TypeScript allows all three, because there
it is the same object. In Velt a conversion would have to make a new object, and with shared
references (semantics stage 2) the difference shows as soon as either object is changed. The
error suggests `{ ...x }`, which makes a copy in TypeScript too. `T` and `Readonly<T>` convert
both ways, because they are the same value.

## `keyof T` and `T[K]`

**Syntax.** `keyof` becomes a contextual keyword in type position: a prefix that binds looser
than postfix `[]` and `[K]`, and tighter than `|`. Indexed access `X[K]` is a postfix in type
position: `[]` with nothing inside is an array, and `[K]` is indexed access. `TypeExprKind`
gains `KeyOf(Box<TypeExpr>)` and `Index(Box<TypeExpr>, Box<TypeExpr>)`. The bound
`K extends keyof T` then parses with no further change.

**Key bounds.** `K extends keyof T` is a new kind of bound, next to interface bounds and
structural bounds. In the body:

- `x[k]` with `x: T` and `k: K` has type `T[K]`, and so does `k: keyof T`, with no `K`
  parameter. `p[k]` on a `Partial<T>` has type `T[K] | null`.
- `T[K]` is opaque. It can be moved, stored, returned and passed to generics. The one exception
  is a literal key: `T["name"]` with `name` in `B(T)` is that field's type.
- `keyof T` and `K` are always unions of literals, so they are copyable, which a `Param` isn't
  today.
- `xs[k]` on an array is unchanged.

**Inference.** A parameter with a key bound is inferred without widening. In
`pluck(users, "name")`, `K` is `"name"`, not `string`, as in TypeScript. Arguments whose
parameter type is a key-bounded slot are checked in the last round of `args.rs`, after the
object slots are solved, so the literal is checked against `keyof User` and not widened first.
TypeScript gives `string[]` for `pluck(users, "name")`, and `(string | number)[]` for
`k: "id" | "name"`. Velt gives the same.

**Run time.**

- A literal type is zero-sized (`velt_vir/src/lower/types.rs`), so `k: K` with `K = "name"`
  costs nothing. `x[k]` becomes the same load as `x.name`.
- With `K = "a" | "b"`, `k` is a union value. `x[k]` becomes a switch on its tag that loads the
  field and wraps it as a member of `T[K]`. The arms are matched by literal value, so the order
  of the union's members doesn't matter.

**Writes.** `x[k] = v` with `v: T[K]` is allowed in the body. When `K` is instantiated with
several keys whose fields have different types, that instantiation is an error. TypeScript
accepts this call and corrupts the object at run time:

```ts ignore
function setField<T, K extends keyof T>(x: T, k: K, v: T[K]) { x[k] = v; }
const r = { id: 1, name: "a" };
const k: "id" | "name" = pick();
setField(r, k, "oops");   // tsc: no error; at run time r is {"id":"oops","name":"a"}
```

Velt can't store a `string` in an `i64` field. The other choice would be a run-time check on
every such write. The error is cheaper and is reported where the call is written. Calls with a
single literal key, which are the common case, are unaffected.

## Errors at instantiation

These requirements are implicit, as in TypeScript, which doesn't ask for `T extends object`:

- `Partial`, `Required`, `Readonly`, `Pick`, `Omit`, `keyof` and `T[K]`: the operand is an
  object type, or for the first three a union of object types.
- `Pick<T, K>`, `T[K]`, and the bound `K extends keyof T`: every member of `K` is a field of `T`.
- `T[K]`: no field named by `K` is `void`, since a union can't hold `void`.
- A write `x[k] = v` through a parameter `K`: the fields named by `K` have one type.

They are checked like generic `Record` keys today, and `record_keys.rs` is generalized into one
pass for all requirements, for `Record` keys and for JSON:

1. Each generic def records its stuck forms (signature, locals, expression types, closures),
   with the span where each one was written.
2. A fixpoint over call sites substitutes each caller's type arguments into its callees'
   requirements, and normalizes them. A requirement that holds is dropped. One that fails is
   reported at the concrete call site, with ``required because `update` uses `Partial<T>` ``
   pointing at the use. One that is still stuck moves to the caller.
3. Instantiations through interfaces and base classes come from `dispatch.rs`. A generic class's
   field types are checked where the class type is resolved with concrete arguments
   (`def_type`).

**Termination.** A requirement that is still stuck and moves to the caller has the same
operators as before, or fewer after simplification. The fixpoint only grows when the call graph
substitutes a parameter with a type built from itself. An example is `f<T>` calling
`f<Pick<T, "a">>`, or `f<T[]>`. Monomorphization can't compile that either, because the set of
instantiations is infinite. The pass detects it, since a def's requirement set keeps growing in
operator depth, and reports
`` `f` calls itself with `Pick<T, "a">`, which instantiates it without end ``. `velt_vir` has no
such check today. The same diagnostic covers the operator-free cases.

**Completeness is not required.** The pass is for errors. Reductions don't come from a table it
fills (see "What `velt_vir` sees"). So a gap in the pass, such as an instantiation path it
doesn't model, is not an ICE. It only means that an invalid instantiation is reported late, by
the reducer, as an `ICE:` that names the requirement. The tests check that the pass and
monomorphization reach the same instantiations.

```text
error: `Partial<T>` needs an object type, found `i64`
   --> src/main.vlt:20:10
    |
 20 |   update(5, {});
    |          ^ `T = i64`
   --> src/lib.vlt:3:33
    |
  3 | function update<T>(x: T, patch: Partial<T>): T {
    |                                 ---------- required because `update` uses `Partial<T>`
```

## What `velt_vir` sees

`velt_vir` can intern types but can't create definitions, and reducing `Partial<User>` creates
an anonymous definition. Sema therefore passes a reducer to lowering:

```rust
pub trait TypeOps {
    /// Reduce a fully concrete operator type; the result mentions no `Op` except `Readonly`.
    fn reduce(&mut self, op: TyOp, args: &[TyId]) -> TyId;
    /// Definitions created by reductions, which `adt_def` falls back to.
    fn def(&self, d: DefId) -> &hir::AdtDef;
}
```

Sema implements it over a copy of the anonymous-type cache, so a shape that sema already made
gets the same `DefId`. `Cx::subst` substitutes an `Op`'s arguments, canonicalizes them (P1), and
calls `reduce`. `Readonly<X>` is laid out as `X`. After lowering, VIR has no operators: layouts,
drop glue, printing and JSON see ordinary anonymous objects, unions and literals.

## Differences from TypeScript

| TypeScript | Velt | Why |
|---|---|---|
| `Partial<number>` is `number`, so `update(5, 6)` type-checks | error: needs an object type | Velt has no mapped types over primitives; a primitive is never what such code means |
| `Pick` and `Omit` of a union | error, with the fix | `keyof` of a union is its common keys in TS, which is rarely intended |
| `T` assignable to `Partial<T>`, `Pick<T, K>`, `Omit<T, K>` | error, suggesting `{ ...x }` | different layouts; a conversion would make a new object |
| a patch can set an optional key to `null` / `undefined` | it can't: `null` is absent for an optional field | Velt has no `undefined` |
| `x[k] = v` with a union `K` writes any member | error at that instantiation | it would store a value of the wrong type |
| classes and `Record` are valid operands | not object types | class fields can be private or getters; `keyof Record<string, V>` is `string` |

## Cost

- **Run time:** none for the types. Every operator is reduced before code is generated. A field
  of a `Partial<T>` is one load, like a written field. `x[k]` is a load, or one switch when `K`
  is a union. The added work is what the source asks for: a null test per optional field that a
  spread reads, and the copies `{ ...x }` makes.
- **Compile time:** one more `TyKind` variant, normalization in `Ctx::subst`, and requirements
  in the existing fixpoint pass. That pass is bounded by the generic defs that mention a stuck
  form.

## Contract changes

These need maintainer sign-off (CLAUDE.md rule 1):

- **HIR** (`hir/mod.rs`, `contracts/hir_encodings.md`):
  - `TyKind::Op` and `TyOp`, and their display;
  - `FieldInfo.optional` as part of anonymous identity (P2);
  - `ExprKind::FieldByName`, `DeferredObject` and `KeyIndex`;
  - every sema pass over HIR handles the new expressions: visit, moves, ownership and flow.
- **AST** (`ast.rs`): `TypeExprKind::KeyOf` and `TypeExprKind::Index`. `velt_fmt`, `velt_doc` and
  `velt_lsp` consume these.
- **`velt_vir`:** `lower` takes `&mut dyn TypeOps`. `Cx::subst` canonicalizes (P1) and reduces.
  The new expressions are lowered.
- **`contracts/sema_ide.md`:** hover and completion show stuck forms, and complete the fields of
  `B(T)` on a `Partial<T>`.

## Diagnostics

- `` `Partial<T>` has no known field `email` ``, with the note "inside `update`, `T` has the
  fields of `{ name: string }`" and the help "add the field to the bound".
- `` `Pick<T, K>` names no fields while `K` is a type parameter ``, with the help "read the field
  with `p[k]`".
- `` a `T` literal may lack the fields of `T` ``, with the uncovered spreads or fields, and the
  help ``spread a value of type `T` first``.
- `` `Partial<U[]>`: `U[]` is not an object type ``, where it is written.
- `` `Partial<T>` needs an object type, found `i64` ``, and `` `"emial"` is not a key of
  `User` `` with the keys in source order, at a concrete call site. Both carry ``required
  because `f` uses …``.
- The `T[K]` write error, with the differing field types in the notes.
- The polymorphic recursion error above.
- ``cannot infer type parameter `T` of `f` ``, when `T` appears only under an operator, gets
  the help `` `T` appears only inside `Partial<T>`; write `f<User>(…)` ``. Operators are not
  inference sites, because `Partial<A>` and `Partial<B>` can be the same type for different `A`
  and `B`.
- `` `T` doesn't convert to `Partial<T>` ``, with the help `copy it: { ...x }`.
- `` cannot assign to `name` through `Readonly<T>` ``.
- The `Pick` / `Omit` / `keyof` union error, with the fix.

## Not proposed

- **Mapped and conditional types** (`{ [P in keyof T]: … }`, `T extends U ? X : Y`). They are
  general type-level programs. The five operators and `keyof` cover the common uses without one,
  and `Op` can take more operators later.
- **Inferring through operators** (TypeScript's reverse mapping for homomorphic mapped types).
- **Re-checking generic bodies per instantiation**, C++ template style. Errors would point into
  library code, compile time would grow with every instantiation, and it would break the rule that
  a generic body is checked once.
- **Classes as operands**, or as types that satisfy structural bounds.

## Decisions

These were open questions in the first draft. TypeScript's behaviour settled them:

1. **A spread copies an optional field only when it is not `null`.** TypeScript copies present
   keys and skips absent ones, and in Velt an absent key is `null` (P2). Non-optional fields are
   always copied, which keeps TypeScript's `null` override.
2. **`Pick<T, K>` and `Omit<T, K>` with a parameter `K` are allowed but name no fields.** This
   matches TypeScript (TS2339). Their fields are read and written through `p[k]`.
3. **Writes through `T[K]` are allowed. An instantiation whose keys have different field types
   is an error.** TypeScript accepts it and corrupts the value. Rejecting writes in generic code
   altogether would also reject the common single-key calls.
