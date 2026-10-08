# Design: utility types and `keyof` on type parameters

Status: accepted with the review in #395 and the owner's decisions of 2026-10-05 and 2026-10-08
([Decisions](#decisions), [Decisions on the revised design](#decisions-on-the-revised-design-2026-10-08));
being implemented in the order of [Implementation order](#implementation-order). Issue #350. It builds
on the merged utility types for concrete object types ([shared-models.md](shared-models.md),
`velt_sema::utility_types`) and on intersections and indexed access on concrete types (#649,
`velt_sema::intersections`), and changes some of their rules;
[Reconciled concrete rules](#reconciled-concrete-rules) lists which. Every TypeScript behaviour
cited here was checked with the `typescript` pinned in `tests/tscompat-oracle` (5.9.3) under its
`tsconfig.base.json` (`strict`), and every run-time one with Node 22.

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

function merge<T, U>(t: T, u: U): T & U {
  return { ...t, ...u };
}
```

Velt resolves every type eagerly. Inside `update`, `T` is `TyKind::Param(0)`: it has no fields,
and only its bounds are known. Generic bodies are checked once, with `T` opaque, and
monomorphization happens later, in `velt_vir`, which has no diagnostics. Today `Partial<T>` is
the error ``Partial` needs a concrete object type; `T` is a type parameter``, and so are `T["k"]`
and `T & U` with a type parameter part (#649 left them to this note).

## Decisions

The owner's decisions on the review's questions (#395, section F, 2026-10-05):

1. **An instance of a generic object type is the object type it spells out.** `Box<number>` with
   `type Box<T> = { v: T }` is `{ v: number }`, as in TypeScript (structural identity). This
   reverses #390's type error (its golden becomes `lang/generic_object_alias_instance`).
2. **An optional field whose type is nullable keeps "absent" apart from `null`**
   (`deletedAt?: string | null`), which is what PATCH APIs need. See P2.
3. **Implicit generic parameters for `Partial`/`Pick`/`Omit` parameters** (review C3) are a
   follow-up: #672.
4. **Writes through `T[K]`:** a write whose value is a read through the same `k` is allowed; any
   other write gets a run-time tag check (it panics exactly where TypeScript would store a value of
   the wrong type), rather than an error at instantiation.

## What changed on `main` since the review

| Change | Effect on this design |
|---|---|
| #583: `?` is part of an object type's shape (`anon::ShapeField`), `hir::FieldDef::optional`, `JSON.stringify` leaves out an absent optional field | The first half of P2 landed. P2 keeps `ShapeField` and changes its `ty` to the declared type (`a?: string \| null` and `a?: string` were one shape), and adds presence, printing and spreads. |
| #649: intersections (`crate::intersections`), branded primitives (`brands.rs`), indexed access on concrete types (`User["name"]`, `TypeExprKind::Indexed`, `Ctx::resolve_indexed`) | `T[K]` needs no new AST node, only the stuck form; `keyof` is the only new type syntax. `T & U` with a parameter part is one more stuck operator (decision 6). `merge_objects` meets declared types under P2, which is TypeScript's rule (`{ a?: string } & { a: string \| null }` has `a: string`). |
| #599: ES private names | `#x` fields are private (`FieldInfo::private_to`), so the operators and `keyof` already leave them out, as TypeScript does (`keyof C` has neither `#x` nor `private`/`protected` members). Nothing to add. |
| #602, #603, #659: closures held in a `const` borrow, frame-allocated environments, local async closures share captures | The new HIR expressions (`FieldByName`, `DeferredObject`, `KeyIndex`) must be visited by the capture, borrow and environment analyses like `Field` and `AdtLit`; `crate::visit` gets the cases and the analyses use it. No new rule. |
| #621 (numrep), #525 (number model) | Operators are reduced before `velt_opt` runs, so numrep sees ordinary loads and switches. A `T[K]` over a `number` field and an `i32` field is the union `number \| i32`, a tagged union like any other, and the tag check of decision 4 compares members by type, so `number` and `i32` stay apart. Literal keys (`pluck(users, "name")` infers `K = "name"`) reuse the literal typing of #525 step 3 rather than adding a second rule. |
| #376: object types that contain themselves (`next?: Node`), boxed by lowering | P1 leaves instances of such types as they are (no canonical form). Operators over them reduce like any object type; `Partial<Node>` keeps `next?: Node`. |
| #336 (`instantiation_cycles.rs`) | Termination reuses it (see [Errors at instantiation](#errors-at-instantiation)). |

## Prerequisites

### P1. Canonical instantiation

Substituting into a generic type produces the type that would have been written directly (#684):

- **Anonymous object types.** `{ a: U }` at `U = string` is `{ a: string }`. Sema's `Ctx::subst`
  re-interns each anonymous object type from its substituted fields, `readonly` and optional flags
  included (R1); inference matches two anonymous defs of one shape field by field; `coerce`
  accepts two forms of one shape. Field-only interfaces' instances (`Pair<string, number>`)
  canonicalize after readonly erasure (`Ctx::same_layout`). Every substitution in sema goes
  through `Ctx::subst`/`subst_known`; `Types::subst` is called directly only inside `anon.rs` and
  `readonly.rs` (step 4 enforces that, R3).
- **One table of shapes.** Sema exports the concrete anonymous def of every shape lowering sees
  (`hir::Program::anon_shapes`, after readonly erasure, so erased defs are never chosen: R2).
  `velt_vir`'s `Cx::canon` maps every instance onto it, so one shape is one VIR type. Anonymous
  defs with the same field names agree on `AdtDef::assigned`.
- **Recursive object types.** An instance of an object type that reaches itself through its
  fields (`interface List<T> { tail?: List<T> }`, #376) stays as it is, in sema and in
  `velt_vir`: canonicalizing it would canonicalize its own fields without end, and its shape
  can't be written without naming it.
- **Symbols.** `velt_vir`'s `type_key` spells anonymous object types structurally
  (`{ a: string }`), so a symbol doesn't depend on which def represents a shape (`velt dev`
  matches functions by symbol).
- **Nested null.** `T | null | null` is `T | null`: `TyTable::intern` makes an option of an option
  the inner option, so a generic `U | null` at `U = string | null` holds `null` as itself, as in
  JavaScript (before, `id<string | null>(null) === null` was `false`; Node prints `true`).
  `velt_vir` treats wrapping and unwrapping a payload that already is the option as the identity:
  `WrapSome`, `UnwrapSome`, `Some` patterns (`??`, narrowing), `Array.pop`, and channels of
  nullable items.
- **Generic unions.** `{ a: U | string }` at `U = i64` is the written `{ a: string | i64 }`, and
  `A | B` at `A = B = string` is `string`. Sema re-canonicalizes a union instance through
  `union_of` and exports the concrete union of each member list (`Program::union_shapes`);
  `velt_vir` maps instances onto it and remaps the generic union's variants by member type
  (`Cx::union_variant`). A generic union instantiated with a member that is itself a union or
  nullable (`U | string` at `U = i64 | bool`) keeps its own variants, because one generic variant
  would spread over several canonical ones; distributing operators over unions (step 3) needs
  that case, and it lands there.

### P2. `?` is a flag over the declared type

A field keeps its declared type `F` and an `optional` flag; both are part of an anonymous type's
identity (`anon::ShapeField`, whose `ty` is the declared type; `FieldInfo::ty` is what a read
gives and `FieldInfo::declared` the written type):

| Declared | Reads as | Layout | Absent vs `null` |
|---|---|---|---|
| `a?: F`, `F` not nullable | `F \| null` | `F \| null` (absent is `null`) | the same value: TypeScript doesn't let it hold `null` |
| `a?: F \| null` | `F \| null` | `F \| null` plus a presence flag after the fields | kept apart |
| `a: F \| null` | `F \| null` | `F \| null` | — |

- `Required` clears the flag and keeps the declared type (`Required<{ a?: string | null }>` is
  `{ a: string | null }`, as in TypeScript; before, `Required` stripped every `| null`).
- A spread copies an optional field only while it is present; a present `null` is copied. This
  fixes the review's C1: `update(u, { deletedAt: null })` clears `deletedAt` and `update(u, {})`
  keeps it, as in Node, and `nick: string | null` patched with `null` is cleared too.
- `JSON.stringify` and `console.log` leave out an absent field and print a present `null`;
  `JSON.parse` records whether the key was there. A class's optional field still shows in
  `console.log`, since Node shows it too (an own key holding `undefined`).
- `{ a?: F }` and `{ a: F | null }` are different types with no implicit conversion between them
  (R5: two types of one layout with a free conversion is the #390 crash), and neither is
  `{ a?: F | null }`, whose layout has the flag. TypeScript relates neither of the first two
  (TS2322 both ways); it does accept `{ a: F | null }` as `{ a?: F | null }` (decision 7).
  The mismatch suggests `{ ...x }`.

Representation: `hir::FieldDef` has `optional` (#583) and gains `presence`. A presence field's
flag is a `bool` stored after the fields of the VIR aggregate (`Cx::presence_slot`, part of the
aggregate's parts, so copy, clone and equality glue carry it). A literal leaves the field absent
with `Intrinsic::FieldAbsent`, any write sets the flag, and spreads read it with
`Intrinsic::FieldPresent`.

Not covered by P2: classes (an optional class field is absent while it is `null`, so a class's
`a?: F | null` doesn't keep the two apart; decision 8), and a generic object type whose
field is `a?: T`, instantiated at a nullable `T` only inside generic code that sema never
substitutes, so that no concrete def of the nullable shape exists: lowering lays that instance
out without the flag. The create-on-demand shape table of step 4 closes this gap.

### P3. Structural field-only bounds

`T extends { name: string; id?: number }`, or a field-only interface bound, means that `T` has at
least these public fields, each assignable to the bound's type, a required field satisfying an
optional one, as TypeScript checks it (`{ v: "s" }` satisfies `{ v: string | number }`). Object
types, field-only interfaces, classes and structs satisfy it (shared-models.md), and so does a
getter, as in TypeScript. Write `B(T)` for the fields the bounds declare. Inside the body,
`x.name` with `x: T` reads and writes the field by name (`ExprKind::FieldByName`).

## Reconciled concrete rules

`utility_types.rs` implements shared-models.md. Where this note differs, TypeScript decides:

| Topic | Rule | Change to what is merged |
|---|---|---|
| Operands | object types, field-only interfaces, classes and structs (public fields), intersections of them, and literal-keyed `Record`s | `Record<"a" \| "b", V>` added |
| `Partial`, `Required`, `Readonly` of a primitive (branded or not) | the primitive (`Partial<number>` is `number`; `update<number>(5, 6)` compiles in TypeScript) | new |
| `Readonly<T[]>` | `T[]` seen read-only (mutating methods are errors, TS2339), inferring `T` through it | new |
| `Omit` with a key that isn't a field | allowed, with a warning | as merged |
| `Required` | clears `?` only (P2) | done with P2 |
| `Readonly<X>` | readonly field flags, erased before lowering like any readonly type (R4) | as merged |
| `Pick`, `Omit`, `keyof` of a union | the common keys; each field's type is the union of the members' types | was an error |
| Indexed access `X[K]` | the union of the fields' types; an optional field contributes `F \| null` | as merged (#649) |
| Class instance → object type | error with the fix-it `{ ...x }` | fix-it changed |

`keyof X` is the union of `X`'s keys as literal types: string literals, and number literals for
numeric keys (`keyof { 0: string; a: number }` is `0 | "a"`). Object types with numeric keys don't
parse yet; until they do, every key is a string literal.

## Stuck operators

### Representation

`TyKind` gets one variant:

```rust
/// A type operator that can't be reduced yet: an argument it inspects is a type parameter or
/// another stuck operator.
Op(TyOp, Vec<TyId>),

pub enum TyOp { Partial, Required, Readonly, Pick, Omit, KeyOf, Index, Intersect, NonNullable, Exclude, Extract, Awaited }
```

- **Stuck** means an inspected argument has a `Param` or a stuck `Op` at its head: the object
  operand of `Partial<T>`, either operand of `T & { b: string }`, or the key operand of
  `Pick<User, K>` and `User[K]`. Everything else reduces where it is written, through
  `utility_types.rs`, `intersections.rs` and `resolve_indexed`. `Partial<{ a: U }>` is
  `{ a?: U }` inside `f<U>`.
- **Every substitution normalizes.** `Types::subst` is renamed `subst_raw` and banned outside
  `anon.rs` and `readonly.rs` (clippy `disallowed-methods`); `Ctx::subst` substitutes,
  canonicalizes (P1) and re-applies `Ctx::op` bottom-up. So no reducible `Op` is ever interned.
- **No silent catch-alls (R3).** Every walker over types (`canon_depth`, `collect_params`,
  `readonly::erase_ty`, `without_error_types`, `has_error`, `has_error_outside_error_types`, and
  `velt_vir`'s `subst_raw`/`canon`) handles `Op` through one shared children/map helper; their
  `_ => t` arms go, so a new variant is a compile error rather than an unsubstituted type at
  lowering, or a `{ p: Partial<T> }` interned as non-generic.
- **Laws** that normalization applies must commute with substitution:
  `norm(subst(norm(t))) == norm(subst(t))` for every law, checked by a property test. So
  `Partial<Partial<T>>` is `Partial<T>`, but `Partial<Readonly<T>>` keeps `readonly`
  (TypeScript: TS2540 on a write through it) and is not `Partial<T>` (R4).
- **Errors.** An operator over `Error` reduces to `Error` without a second diagnostic, and
  `has_error` looks inside `Op`.

### Member access in the generic body

A stuck `Partial`, `Required`, `Pick`, `Omit` or `Intersect` value is an anonymous object once
reduced, so its fields are read and written by name (`ExprKind::FieldByName`); a `Readonly` one
is read only. `F` and `opt` are the field's type and optionality in `B(T)`:

| Type of `p` | Fields `p.f` can name | Type of `p.f` | `p.f = v` |
|---|---|---|---|
| `T` | `B(T)` | `F`, or `F \| null` if `opt` | yes |
| `Partial<T>` | `B(T)` | `F \| null` | yes |
| `Required<T>` | `B(T)` | `F` | yes |
| `Readonly<T>`, `Partial<Readonly<T>>` | `B(T)` | as for `T` / `Partial<T>` | no |
| `Pick<T, K>`, `K` a literal union | `B(T)` ∩ `K` | as for `T` | yes |
| `Omit<T, K>`, `K` a literal union | `B(T)` minus `K` | as for `T` | yes |
| `T & U` | `B(T)` ∪ `B(U)` ∪ written fields | the meet of the parts' types | yes |
| `Pick<T, K>` or `Omit<T, K>`, `K` a parameter | none (TypeScript: TS2339) | — | — |

Reading a non-copyable field through `FieldByName` or `x[k]` shares it; a partial move by name is
an error.

**Sharing (R6).** `assigned_fields.rs` sees writes through generic code too: a `FieldByName`
write marks every anonymous def with a field of that name, an `x[k]` write through a parameter
`K` marks every anonymous def, and defs the reducer creates take part. Writes inside closures are
found through `crate::visit`, as for `Field` today.

### Object literals and spreads at a deferred type

`{ ...x, ...patch }` typed as `T` is checked symbolically and emitted as
`ExprKind::DeferredObject { ty, parts }`, each part with its use mode (move, share or copy), so
the moves pass decides for the whole value. `velt_vir` expands it after substitution into the
merged struct `spread.rs` builds for a concrete type, presence tests included (P2), before drop
elaboration and async lowering.

What a literal must provide depends on its target:

| Target | Must cover |
|---|---|
| `T`, `Readonly<T>` | `keyof T`; optional fields may be left out |
| `Required<T>` | `keyof T` |
| `Pick<T, K>` | `K` |
| `Omit<T, K>` | `keyof T` minus `K` |
| `T & U` | `keyof T` and `keyof U` |
| `Partial<T>` | nothing (`draft: Partial<T> = {}`) |

`...x` with `x: T`, `Required<T>` or `Readonly<T>` covers `keyof T`; `...p` with `p: Pick<T, K>`
covers `K`; `Omit<T, K>` covers `keyof T` minus `K`; `Partial<T>` covers nothing and may override
(P2's presence rule); `name: e` covers `name`. `{ ...p }` with `p: Partial<T>` as a `T` is an
error, as in TypeScript (TS2322).

- `{ ...x }` with `x: T | null` is allowed and gives `{}` for `null`, as in TypeScript and Node;
  a literal `{ ...null }` stays an error (TypeScript: TS2698).
- `function copy<T>(x: T): T { return { ...x }; }` compiles in TypeScript and requires `T` to be
  an object type in Velt, although no operator is written: it is recorded as a requirement
  ([Errors at instantiation](#errors-at-instantiation)).
- Object rest at a deferred type, `const { id, ...rest } = x` with `x: T`, gives
  `rest: Omit<T, "id">` through the same lowering (TypeScript types it that way).

### `keyof T` and `T[K]`

- **Syntax:** `keyof` is a contextual prefix in type position, looser than postfix `[]` and `[K]`
  and tighter than `&` and `|` (`keyof X[]` is the keys of an array). `TypeExprKind::KeyOf` is
  the one new AST node; `X[K]` is the merged `TypeExprKind::Indexed` (#649), which today accepts
  only a concrete object and literal keys. `T[number]` indexes arrays and tuples.
- **Key bounds:** `K extends keyof T` is a new bound kind. `x[k]` with `k: K` (or `k: keyof T`)
  has type `T[K]` (`ExprKind::KeyIndex`), opaque in the body except with a literal key
  (`T["name"]` is the field's type). Key types are always literal unions, so they are copyable.
  `K extends keyof T` with `T` a union uses the common keys (`get(uu, "kind")` compiles in
  TypeScript).
- **Inference** keeps literal keys: `pluck(users, "name")` infers `K = "name"` and returns
  `string[]`, and a key of type `"id" | "name"` returns `(string | number)[]`, as in TypeScript.
  Arguments of key-bounded slots are checked in the last round of `args.rs`, after the object
  slots.
- **Run time:** a single-key `K` is zero-sized and `x[k]` is a load; a union `K` is a tag and
  `x[k]` one switch.
- **Writes** follow decision 4: `dst[k] = src[k]` (a read through the same immutable `k`) is
  always allowed, so `copyField` and the standard `pick` body compile; any other `x[k] = v`
  checks in the switch that `v`'s member matches the field and panics otherwise. TypeScript
  accepts `setField(r, k, "oops")` with `k: "id" | "name"` and stores `{"id":"oops","name":"a"}`;
  Velt panics there. A write of a value that isn't a `T[K]` (`x[k] = 1`) is an error in both
  (TS2322).
- **`pick`'s body:** TypeScript writes `const r = {} as Pick<T, K>; for (const k of ks)
  r[k] = o[k]; return r;`. Velt can't create a `Pick<T, K>` with no fields set; decision 5 says
  how it compiles.

### Inference through operators

As in TypeScript, with its canonical choice where several `T` fit (all checked with tsc):

- `p: Partial<T>` with `{ a: 1 }` gives `T = { a: number }` (the argument with `?` cleared);
- `Readonly<T>`, `Required<T>`: `T` is the argument; `Readonly<U[]>` gives `U`;
- `Pick<T, K>`: `T` is the argument and `K` its keys;
- `Record<K, V>`: `K` and `V` from a literal-keyed record.

### Errors at instantiation

Requirements are implicit (TypeScript doesn't ask for `T extends object`): an operand is an
object type (or a primitive for `Partial`/`Required`/`Readonly`), keys are fields, the parts of an
intersection meet, `T[K]` has no `void` field, and a spread `{ ...x }` at a `T` needs an object
type. They are checked by one generalized pass, onto which `record_keys` and the JSON pass move:
each generic def records its stuck forms with spans; a fixpoint over call sites substitutes and
normalizes them; a failure is reported at the concrete call site with
``required because `update` uses `Partial<T>` ``; a still stuck one moves to the caller.

**Termination** reuses `instantiation_cycles.rs` (#336), which rejects every growing edge in a
strongly connected component, so the fixpoint terminates without an operator-depth detector.
`f<T>` calling `f<Partial<T>>` is finite after simplification (TypeScript compiles it); an edge
whose argument normalizes to a form already seen on that cycle is marked as not growing.

**Completeness is not required.** Reductions don't come from a table the pass fills, so a gap in
the pass reports an invalid instantiation late, as an `ICE:` from the reducer that names the
requirement. The tests check that the pass and monomorphization reach the same instantiations.

### What `velt_vir` sees

The reducer's state lives in `hir::Program`, since sema's `Ctx` is gone when `lower` runs (R7):

- **One shared, create-on-demand shape table** extends `Program::anon_shapes` (P1): `velt_vir`
  asks it for the def of a shape, and a shape it doesn't have is created in an append-only owned
  overlay of defs (DefIds after `hir.defs.len()`), read through one `Cx::def` accessor that
  replaces the direct `hir.def(` calls. There is one table, not a velt_vir table and a copy of
  sema's cache, so a shape is never two types.
- `Cx::subst` substitutes an `Op`'s arguments, canonicalizes, and reduces through the table. After
  lowering there are no operators: layouts, drop glue, printing and JSON see ordinary anonymous
  objects, unions and literals, and so do `velt_opt` and numrep.

## C6: forms used together with these

In order: `NonNullable<T>`, `Exclude`/`Extract<T, U>` (a union filter; the object-pattern form
reuses P3), `Record<keyof T, V>` and `Record<K, V>` with a parameter `K`, `T[number]`,
`Awaited<T>`, object rest at a `T`, then `typeof x` / `keyof typeof X` /
`(typeof X)[keyof typeof X]` (which need value lookup and `as const` literal types). All compile
in TypeScript. `ReturnType`/`Parameters` belong with #209.

## Differences from TypeScript

| TypeScript | Velt | Why |
|---|---|---|
| `T` assignable to `Partial<T>`, `Pick<T, K>`, `Omit<T, K>` (same object) | in storage positions an error suggesting `{ ...x }`; parameters: #672 | different layouts; a conversion makes a new object |
| `{ a: F \| null }` assignable to `{ a?: F \| null }` | error suggesting `{ ...x }` (decision 7) | the second has a presence flag |
| `x[k] = v` with a union `K` stores any member | panics when the member doesn't match | it would store a value of the wrong type |
| a patch `{ name: undefined }` removes `name` | not expressible; `null` is absent only for `a?: F` without `null` | Velt has no `undefined` |
| an optional class field without an initializer is an own key holding `undefined`, so spreading the instance clears the target's field | it is absent and isn't copied | Velt has no `undefined` |

## Cost

- **Run time:** none for the types. A field of a `Partial<T>` is one load. `x[k]` is a load, or one
  switch for a union `K`. Added work is what the source asks for: a presence test per optional
  field a spread reads, the tag check of decision 4, and copies `{ ...x }` makes. A presence
  field costs one byte in its object.
- **Compile time:** one `TyKind` variant, normalization in `Ctx::subst`, requirements in the
  existing fixpoint pass.

## Contract changes

- **HIR** (`hir/mod.rs`, `contracts/hir_encodings.md`): `TyKind::Op`/`TyOp` and their display;
  `FieldDef::presence` and `Intrinsic::FieldAbsent`/`FieldPresent` (P2); `ExprKind::FieldByName`,
  `DeferredObject` (with per-part use modes) and `KeyIndex`; `Program::anon_shapes` and
  `union_shapes` (P1) growing into the shared shape table; `TyTable::intern` collapsing nested
  options (P1).
- **AST** (`ast.rs`): `TypeExprKind::KeyOf`. Consumers: sema `ast_walk.rs`, `velt_lsp`
  (`index/scope/mod.rs`, `inlay_hints.rs`), `velt_doc` `sig.rs`, `velt_fmt` `print/types.rs`,
  the `velt_syntax` test tree printer.
- **`velt_vir`:** one `Cx::def` accessor over the overlay; `Cx::subst` canonicalizes (P1) and
  reduces.
- **Sema:** `Types::subst` → `subst_raw`, restricted.
- **IDE** (`contracts/sema_ide.md`): the stuck form in a body, the reduced form at an
  instantiated call; completion offers `B(T)` on a `Partial<T>`.

## Diagnostics

- `` `Partial<T>` has no known field `email` ``, with the note "inside `update`, `T` has the
  fields of `{ name: string }`" and the help "add the field to the bound".
- `` `Pick<T, K>` names no fields while `K` is a type parameter ``, help "read the field with
  `p[k]`".
- `` a `T` literal may lack the fields of `T` ``, with the uncovered spreads or fields, help
  ``spread a value of type `T` first``.
- `` `Partial<T>` needs an object type, found `string[]` `` and `` `"emial"` is not a key of
  `User` `` (keys in source order) at a concrete call site, with ``required because `f` uses …``.
- `` `T` doesn't convert to `Partial<T>` ``, help `copy it: { ...x }` (and #672 once it lands).
- `` cannot assign to `name` through `Readonly<T>` ``.
- The tag-check panic: `` `x[k] = v` stores a `string` in field `id` (`number`) ``.
- ``cannot infer type parameter `T` of `f` `` when `T` appears only under an operator that
  doesn't infer (`Omit<T, K>`), help `` write `f<User>(…)` ``.

## Implementation order

1. **P1 on `main`:** R1, R2, nested `T | null`, generic unions with plain members, structural
   `type_key`, the exported shape tables, decision 1.
2. **P2** with the presence flag (decision 2). Steps 1 and 2 are one pull request, #684.
3. **Concrete operators** per [Reconciled concrete rules](#reconciled-concrete-rules), and
   generic unions with union or nullable members.
4. **Stuck operators, `FieldByName`, `DeferredObject`**, with R3, R6, the overlay (which closes
   P2's generic gap), termination. Waits for #525 step 3, which rewrites sema's number typing in
   the same files.
5. **`keyof`/`T[K]`** with decision 4's writes, and inference through operators.
6. **C6's forms.**
7. **#672: implicit generic parameters.**

## Decisions on the revised design (2026-10-08)

The owner decided the revision's open questions, all as recommended; the design is approved with
these answers:

5. **`pick`'s body.** `{} as D`, where `D` is a deferred object type, is allowed only as the
   initializer of a local. It is stored as `Partial<D>`, `r[k] = …` writes set fields, and its
   first other use converts it with a run-time check that every field is present; a missing key
   is a run-time error that names it (TypeScript would hand out an object without it). The
   standard `pick` compiles unchanged, at one presence test per field at the conversion.
6. **`T & U` with a type parameter part** (`merge<T, U>(t: T, u: U): T & U`), which #649 left to
   #350, is a deferred operator, `TyOp::Intersect`, reduced by `crate::intersections` at
   instantiation, in step 4; the spread is a `DeferredObject` like `update`'s.
7. **`{ a: F | null }` where `{ a?: F | null }` is expected**, which TypeScript accepts, stays an
   error with the `{ ...x }` fix-it, like the other storage-position conversions (#672 covers
   parameters); types.md lists it as a difference from TypeScript.
8. **Classes and `a?: F | null`:** no presence flag. A class's fields always exist in its fixed
   layout, and Node's own key holding `undefined` is modelled by neither "absent" nor `null`.

## Review checklist (#395, 2026-10-03)

| Point | Where it is addressed |
|---|---|
| R1 readonly flags dropped by canonicalization | P1, done |
| R2 erased readonly defs chosen by lowering | P1 (`Program::anon_shapes`), done |
| R3 catch-all arms in type walkers; raw `subst` | Stuck operators: no catch-alls, `subst_raw` banned; raw calls already converted in P1 |
| R4 a permanent `Readonly` operator; the `Partial<Readonly<T>>` law | `Readonly` is field flags; laws commute with substitution, property test |
| R5 `{ a?: F }` and `{ a: F \| null }` with one layout | P2: different types, no conversion; presence flag for `a?: F \| null` |
| R6 writes through generic code invisible to sharing | Member access: sharing |
| R7 the `TypeOps` reducer | What `velt_vir` sees: state in `Program`, one table, overlay, `Cx::def`; structural `type_key` (P1, done) |
| Nested `T \| null`; generic unions | P1, done (union or nullable members: step 3) |
| `DeferredObject` use modes; `copy<T>` requirement | Object literals and spreads |
| C1 `update(u, { deletedAt: null })` loses `null` | P2, done |
| C2 TypeScript-compatible alternatives | Reconciled concrete rules; writes: decision 4 |
| C3 `User` as `Partial<User>` | #672 |
| C4 inference through operators | Inference through operators |
| C5 termination | Errors at instantiation |
| C6 missing forms | C6 |
| C7 numeric keys, P3 strictness, `{ ...x }` of `T \| null`, class optional fields, `pick` | `keyof`, P3, spreads, differences table, decision 5 |
| D contract changes | Contract changes |

## Not proposed

- Mapped and conditional types (`{ [P in keyof T]: … }`, `T extends U ? X : Y`).
- Re-checking generic bodies per instantiation (C++ templates).
