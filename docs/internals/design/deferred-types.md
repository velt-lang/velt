# Design: utility types and `keyof` on type parameters

Status: accepted with the review in #395, being implemented in the order of
[Implementation order](#implementation-order). Issue #350. It builds on the merged utility types
for concrete object types ([shared-models.md](shared-models.md), `velt_sema::utility_types`) and
changes some of their rules; [Reconciled concrete rules](#reconciled-concrete-rules) lists which.
The TypeScript behaviour cited here was checked with `tsc` 6.0 and 7.0 `--strict` and Node 22.

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
and only its bounds are known. Generic bodies are checked once, with `T` opaque, and
monomorphization happens later, in `velt_vir`, which has no diagnostics. Today `Partial<T>` is
the error ``Partial` needs a concrete object type; `T` is a type parameter``.

## Decisions

The owner's decisions on the review's questions (#395, section F):

1. **An instance of a generic object type is the object type it spells out.** `Box<number>` with
   `type Box<T> = { v: T }` is `{ v: number }`, as in TypeScript. This reverses #390's type error
   (its golden is now `lang/generic_object_alias_instance`).
2. **An optional field whose type is nullable keeps "absent" apart from `null`**
   (`deletedAt?: string | null`), which is what PATCH APIs need. See P2.
3. **Implicit generic parameters for `Partial`/`Pick`/`Omit` parameters** (C3) come as a
   follow-up.
4. **Writes through `T[K]`:** a write whose value is a read through the same `k` is allowed;
   any other write gets a run-time tag check (it panics exactly where TypeScript would store a
   value of the wrong type), rather than an error at instantiation.

## Prerequisites

### P1. Canonical instantiation (done)

Substituting into a generic type produces the type that would have been written directly:

- **Anonymous object types.** `{ a: U }` at `U = string` is `{ a: string }`. Sema's `Ctx::subst`
  re-interns each anonymous object type from its substituted fields, `readonly` flags included
  (`anon.rs`); inference matches two anonymous defs of one shape field by field; `coerce` accepts
  two forms of one shape. Field-only interfaces' instances (`Pair<string, number>`) canonicalize
  after readonly erasure (`Ctx::same_layout`).
- **One table of shapes.** Sema exports the concrete anonymous def of every shape lowering sees
  (`hir::Program::anon_shapes`, after readonly erasure, so erased defs are never chosen).
  `velt_vir`'s `Cx::canon` maps every instance onto it, so one shape is one VIR type.
  Anonymous defs with the same field names agree on `AdtDef::assigned`.
- **Symbols.** `velt_vir`'s `type_key` spells anonymous object types structurally
  (`{ a: string }`), so a symbol doesn't depend on which def represents a shape (`velt dev`
  matches functions by symbol).
- **Nested null.** `T | null | null` is `T | null`: `TyTable::intern` makes an option of an option
  the inner option, so a generic `U | null` at `U = string | null` holds `null` as itself, as in
  JavaScript (before, `id<string | null>(null) === null` was `false`). `velt_vir` treats wrapping
  and unwrapping a payload that already is the option as the identity: `WrapSome`, `UnwrapSome`,
  `Some` patterns (`??`, narrowing), `Array.pop`, and channels of nullable items.
- **Generic unions.** `{ a: U | string }` at `U = i64` is the written `{ a: string | i64 }`, and
  `A | B` at `A = B = string` is `string`. Sema re-canonicalizes a union instance through
  `union_of` and exports the concrete union of each member list (`Program::union_shapes`);
  `velt_vir` maps instances onto it and remaps the generic union's variants by member type
  (`Cx::union_variant`: injections, `UnwrapVariant`, variant patterns, switch keys), a variant of a
  union that collapsed to one member being the value itself. Inference matches a union against
  a union member by member. Still open: a generic union instantiated with a member that is a union
  or nullable (`U | string` at `U = i64 | bool`) keeps its own variants, because one generic
  variant would spread over several canonical ones.

Regression tests: `lang/anon_generic_instantiation`, `lang/generic_object_alias_instance`,
`lang/nullable_generic_payload`, `lang/generic_union_instance`.

### P2. `?:` is a flag, not a type

Today `name?: T` is `name: T | null`. Instead, a field keeps its declared type `F` and an
`optional` flag, and the flag is part of an anonymous type's identity:

| Declared | Reads as | Layout | Absent vs `null` |
|---|---|---|---|
| `a?: F`, `F` not nullable | `F \| null` | `F \| null` (absent is `null`) | the same value: TypeScript doesn't allow an explicit `null` here |
| `a?: F \| null` | `F \| null` | `{ present: bool, value: F \| null }` | kept apart |
| `a: F \| null` | `F \| null` | `F \| null` | — |

- `Required` clears the flag and keeps the declared type (TypeScript: `Required<{ a?: string | null }>`
  is `{ a: string | null }`).
- A spread copies an optional field only when it is present; a present `null` is copied. So
  `update(u, { deletedAt: null })` clears `deletedAt`, as in Node, and `update(u, {})` keeps it.
- JSON and `console.log` omit an absent field and print a present `null`.
- `{ a?: F }` and `{ a: F | null }` are different types with no implicit conversion between them
  (the #390 lesson: two types of one layout with a free conversion crash lowering). The error
  suggests `{ ...x }`. `hir::FieldDef` gains the `optional` flag, and lowering keeps the types
  apart.
- The reference's "`a?: T` is `T | null` everywhere" (types.md) is rewritten.

### P3. Structural field-only bounds

`T extends { name: string; id?: number }`, or a field-only interface bound, means that `T` has
at least these public fields, each assignable to the bound's type, a required field satisfying an
optional one, as TypeScript checks it. Object types, field-only interfaces, classes and structs
satisfy it (shared-models.md). Write `B(T)` for the fields the bounds declare. Inside the body,
`x.name` with `x: T` reads and writes the field by name (`ExprKind::FieldByName`).

## Reconciled concrete rules

`utility_types.rs` implements shared-models.md. Where this note differs, TypeScript decides:

| Topic | Rule | Change to what is merged |
|---|---|---|
| Operands | object types, field-only interfaces, classes and structs (public fields), and literal-keyed `Record`s | `Record<"a" \| "b", V>` added |
| `Partial`, `Required`, `Readonly` of a primitive | the primitive (`Partial<number>` is `number`) | new |
| `Readonly<T[]>` | `T[]` seen read-only, inferring through it | new |
| `Omit` with a key that isn't a field | allowed, with a warning ("`emial` is not a field of `User`") | was an error |
| `Required` | clears `?` only (P2) | cleared every `\| null` |
| `Readonly<X>` | readonly field flags, erased before lowering like any readonly type | as merged |
| `Pick`, `Omit`, `keyof` of a union | the common keys; each field's type is the union of the members' types | was an error |
| Class instance → object type | error with the fix-it `{ ...x }` | fix-it changed |

`keyof X` is the union of `X`'s keys as literal types: string literals, and number literals for
numeric keys (`keyof { 0: string }` contains `0`).

## Stuck operators

### Representation

`TyKind` gets one variant:

```rust
/// A type operator that can't be reduced yet: an argument it inspects is a type parameter or
/// another stuck operator.
Op(TyOp, Vec<TyId>),

pub enum TyOp { Partial, Required, Readonly, Pick, Omit, KeyOf, Index, NonNullable, Exclude, Extract, Awaited }
```

- **Stuck** means an inspected argument has a `Param` or a stuck `Op` at its head: the object
  operand of `Partial<T>`, or the key operand of `Pick<User, K>` and `User[K]`. Everything else
  reduces through `utility_types.rs` where it is written. `Partial<{ a: U }>` is `{ a?: U }` inside
  `f<U>`.
- **Every substitution normalizes.** `Types::subst` is renamed `subst_raw` and banned outside
  `anon.rs` (clippy `disallowed-methods`); `Ctx::subst` substitutes, canonicalizes (P1) and
  re-applies `Ctx::op` bottom-up. So no reducible `Op` is ever interned.
- **No silent catch-alls.** Every walker over types (`children`, `map`, `canon`, `collect_params`,
  `readonly::erase_ty`, `without_error_types`, `has_error`, and `velt_vir`'s `subst_raw`/`canon`)
  handles `Op` through one shared helper; their `_ => t` arms go, so a new variant is a compile
  error rather than an unsubstituted type at lowering.
- **Laws** that normalization applies must commute with substitution:
  `norm(subst(norm(t))) == norm(subst(t))` for every law, checked by a property test. So
  `Partial<Partial<T>>` is `Partial<T>`, but `Partial<Readonly<T>>` keeps `readonly` (TypeScript
  does).
- **Errors.** An operator over `Error` reduces to `Error` without a second diagnostic.

### Member access in the generic body

A stuck `Partial`, `Required`, `Pick` or `Omit` value is an anonymous object once reduced, so its
fields are read and written by name (`ExprKind::FieldByName`). A `Readonly` one is read only.
`F` and `opt` are the field's type and optionality in `B(T)`:

| Type of `p` | Fields `p.f` can name | Type of `p.f` | `p.f = v` |
|---|---|---|---|
| `T` | `B(T)` | `F`, or `F \| null` if `opt` | yes |
| `Partial<T>` | `B(T)` | `F \| null` | yes |
| `Required<T>` | `B(T)` | `F` | yes |
| `Readonly<T>` | `B(T)` | as for `T` | no |
| `Pick<T, K>`, `K` a literal union | `B(T)` ∩ `K` | as for `T` | yes |
| `Omit<T, K>`, `K` a literal union | `B(T)` minus `K` | as for `T` | yes |
| `Pick<T, K>` or `Omit<T, K>`, `K` a parameter | none (TypeScript: TS2339) | — | — |

Reading a non-copyable field through `FieldByName` or `x[k]` shares it; a partial move by name is
an error.

**Sharing (R6).** `assigned_fields.rs` sees writes through generic code too: a `FieldByName`
write marks every anonymous def with a field of that name, an `x[k]` write through a parameter
`K` marks every anonymous def, and defs the reducer creates take part.

### Object literals and spreads at a deferred type

`{ ...x, ...patch }` typed as `T` is checked symbolically and emitted as
`ExprKind::DeferredObject { ty, parts }`, each part with its use mode (move, share or copy), so
the moves pass decides for the whole value. `velt_vir` expands it after substitution into the
merged struct `spread.rs` builds for a concrete type, before drop elaboration and async lowering.

What a literal must provide depends on its target:

| Target | Must cover |
|---|---|
| `T`, `Readonly<T>` | `keyof T`; optional fields may be left out |
| `Required<T>` | `keyof T` |
| `Pick<T, K>` | `K` |
| `Omit<T, K>` | `keyof T` minus `K` |
| `Partial<T>` | nothing (`draft: Partial<T> = {}`) |

`...x` with `x: T`, `Required<T>` or `Readonly<T>` covers `keyof T`; `...p` with `p: Pick<T, K>`
covers `K`; `Omit<T, K>` covers `keyof T` minus `K`; `Partial<T>` covers nothing and may override
(P2's presence rule); `name: e` covers `name`. `{ ...x }` with `x: T | null` is allowed
(`{ ...null }` is `{}`), and `function copy<T>(x: T): T { return { ...x }; }` requires `T` to be an
object type although no operator is written.

Object rest at a deferred type, `const { id, ...rest } = x` with `x: T`, gives `rest: Omit<T, "id">`
through the same lowering.

### `keyof T` and `T[K]`

- **Syntax:** `keyof` is a contextual prefix in type position, looser than postfix `[]` and `[K]`
  and tighter than `|` (`keyof X[]` is the keys of an array). `X[K]` is a postfix; `T[number]`
  indexes arrays and tuples.
- **Key bounds:** `K extends keyof T` is a new bound kind. `x[k]` with `k: K` (or `k: keyof T`)
  has type `T[K]`, opaque in the body except with a literal key (`T["name"]` is the field's
  type). Key types are always literal unions, so they are copyable.
- **Inference** keeps literal keys: `pluck(users, "name")` infers `K = "name"`; arguments of
  key-bounded slots are checked in the last round of `args.rs`, after the object slots.
- **Run time:** a single-key `K` is zero-sized and `x[k]` is a load; a union `K` is a tag and
  `x[k]` one switch.
- **Writes** follow decision 4: `dst[k] = src[k]` (a read through the same immutable `k`) is
  always allowed, so `copyField` and the standard `pick` body compile; any other `x[k] = v`
  checks in the switch that `v`'s member matches the field and panics otherwise.
- **`pick`'s body** (open): TypeScript writes `const r = {} as Pick<T, K>; for (…) r[k] = o[k];`.
  Velt has no `as` for this, and `{}` doesn't cover `K`. The candidate is building a
  `Partial<Pick<T, K>>` and converting it with a run-time check that every key was set; it is
  decided in step 5.

### Inference through operators

As in TypeScript, with its canonical choice where several `T` fit:

- `p: Partial<T>` with `{ a: 1 }` gives `T = { a: number }` (the argument with `?` cleared);
- `Readonly<T>`, `Required<T>`: `T` is the argument;
- `Pick<T, K>`: `T` is the argument and `K` its keys;
- `Record<K, V>`: `K` and `V` from a literal-keyed record.

### Errors at instantiation

Requirements are implicit (TypeScript doesn't ask for `T extends object`): an operand is an
object type (or a primitive for `Partial`/`Required`/`Readonly`), keys are fields, `T[K]` has no
`void` field. They are checked by one generalized pass, onto which `record_keys` and the JSON pass
move: each generic def records its stuck forms with spans; a fixpoint over call sites
substitutes and normalizes them; a failure is reported at the concrete call site with
``required because `update` uses `Partial<T>` ``; a still stuck one moves to the caller.

**Termination** reuses `instantiation_cycles.rs` (#336), which rejects every growing edge in a
strongly connected component. `f<T>` calling `f<Partial<T>>` is finite after simplification; an
edge whose argument normalizes to a form already seen is marked as not growing.

### What `velt_vir` sees

The reducer's state lives in `hir::Program`, since sema's `Ctx` is gone when `lower` runs:

- **One shared, create-on-demand shape table** extends `Program::anon_shapes` (P1): `velt_vir`
  asks it for the def of a shape, and a shape it doesn't have is created in an append-only owned
  overlay of defs (DefIds after `hir.defs.len()`), read through one `Cx::def` accessor that
  replaces the direct `hir.def(` calls.
- `Cx::subst` substitutes an `Op`'s arguments, canonicalizes, and reduces through the table.
  After lowering there are no operators.

## C6: forms used together with these

In order: `NonNullable<T>`, `Exclude`/`Extract<T, U>` (a union filter; the object-pattern form
reuses P3), `Record<keyof T, V>` and `Record<K, V>` with a parameter `K`, `T[number]`,
`Awaited<T>`, object rest at a `T`, then `typeof x` / `keyof typeof X` (which need value lookup
and `as const` literal types). `ReturnType`/`Parameters` belong with #209.

## C3: implicit generic parameters (follow-up)

`toDto(u): Pick<User, "id" | "name"> { return u; }` and entities passed as patches compile in
TypeScript and keep the same object (`toDto(u) === u`). For **parameters**, a
`p: Partial<User>` / `Pick<…>` / `Omit<…>` becomes an implicit generic `<S satisfying it>(p: S)`
and is monomorphized, so the callee receives the same object. Storage positions
(`Partial<User>[]`, fields) keep the `{ ...x }` error.

## Differences from TypeScript

| TypeScript | Velt | Why |
|---|---|---|
| `T` assignable to `Partial<T>`, `Pick<T, K>`, `Omit<T, K>` in storage positions | error, suggesting `{ ...x }` (parameters: C3) | different layouts; a conversion makes a new object |
| `{ a: F \| null }` assignable to `{ a?: F \| null }` | error, suggesting `{ ...x }` | different layouts (P2) |
| `x[k] = v` with a union `K` stores any member | panics when the member doesn't match | it would store a value of the wrong type |
| an optional class field without an initializer is an own key holding `undefined` | it is absent | Velt has no `undefined` |

## Cost

- **Run time:** none for the types. A field of a `Partial<T>` is one load. `x[k]` is a load, or one
  switch for a union `K`. Added work is what the source asks for: a presence test per optional
  field a spread reads, the tag check of decision 4, and copies `{ ...x }` makes.
- **Compile time:** one `TyKind` variant, normalization in `Ctx::subst`, requirements in the
  existing fixpoint pass.

## Contract changes

- **HIR** (`hir/mod.rs`, `contracts/hir_encodings.md`): `TyKind::Op`/`TyOp` and their display;
  `FieldDef` `optional` and `readonly` flags; `ExprKind::FieldByName`, `DeferredObject` (with per-part
  use modes) and `KeyIndex`; `Program::anon_shapes` (done) growing into the shared shape table;
  `TyTable::intern` collapsing nested options (done).
- **AST** (`ast.rs`): `TypeExprKind::KeyOf` and `Index`. Consumers: sema `ast_walk.rs`,
  `velt_lsp` (`index/scope/mod.rs`, `inlay_hints.rs`), `velt_doc` `sig.rs`, `velt_fmt`
  `print/types.rs`, the `velt_syntax` test tree printer.
- **`velt_vir`:** one `Cx::def` accessor over the overlay; `Cx::subst` canonicalizes (done) and
  reduces; JSON and format glue for optional fields.
- **Sema:** `Types::subst` → `subst_raw`, restricted.
- **IDE** (`contracts/sema_ide.md`): the stuck form in a body, the reduced form at an
  instantiated call.
- **Goldens:** #390's error golden became `lang/generic_object_alias_instance` (done).

## Implementation order

1. **P1 on `main`** — done: R1 (readonly flags), R2 (erased defs), nested `T | null`, generic
   unions with plain members, structural `type_key`, the exported shape tables, decision 1.
2. **P2** with the presence-flag representation (decision 2).
3. **Concrete operators** per [Reconciled concrete rules](#reconciled-concrete-rules).
4. **Stuck operators, `FieldByName`, `DeferredObject`**, with R3, R6, the overlay, termination.
5. **`keyof`/`T[K]`** with decision 4's writes, and inference through operators.
6. **C6's forms.**
7. **C3's implicit generic parameters.**

## Not proposed

- Mapped and conditional types (`{ [P in keyof T]: … }`, `T extends U ? X : Y`).
- Re-checking generic bodies per instantiation (C++ templates).
