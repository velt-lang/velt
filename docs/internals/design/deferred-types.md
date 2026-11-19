# Utility types and `keyof` on type parameters

Status: partly implemented. Issues: [#350](https://github.com/velt-lang/velt/issues/350), [#672](https://github.com/velt-lang/velt/issues/672).

## Summary

TypeScript's type-level forms (utility types, `keyof`, indexed access `T[K]`, intersections,
mapped and conditional types) are mostly used in generic code, where an argument is a type
parameter. Velt checks a generic body once, with its type parameters opaque, and monomorphizes
later. A form that needs a type parameter is therefore a **deferred type**: it stays symbolic in
the generic body, is checked there through the parameter's bounds, and is reduced to an ordinary
type at every instantiation. After lowering no deferred type remains, so layouts, glue, printing,
JSON and the optimizer see plain object types, unions and literals.

The concrete forms are in [shared-models.md](shared-models.md) (utility types on concrete
types) and the Reference.

## Motivation

```ts
function update<T extends object>(x: T, patch: Partial<T>): T {
  return { ...x, ...patch };
}

function pluck<T, K extends keyof T>(xs: T[], k: K): T[K][] {
  return xs.map((x) => x[k]);
}

function merge<T extends object, U extends object>(t: T, u: U): T & U {
  return { ...t, ...u };
}

const u = update({ id: 1, name: "a" }, { name: "b" });
const names = pluck([u], "name");
const m = merge({ a: 1 }, { b: "x" });
console.log(JSON.stringify(u), names, m.a, m.b);
```

## Design

### Canonical instantiation

Substituting into a generic type gives the type that would have been written directly:

- **Object types.** `{ a: U }` at `U = string` is `{ a: string }`; an instance of a generic
  object type or field-only interface (`Box<number>` with `type Box<T> = { v: T }`) is the
  object type it spells out, as in TypeScript's structural identity. Every substitution in sema
  goes through `Ctx::subst`, which re-interns anonymous object types from their substituted
  fields, `readonly` and optional flags included. Sema exports the concrete definition of every
  shape lowering sees (`hir::Program::anon_shapes`, after readonly erasure), and `velt_vir` maps
  every instance onto it, so one shape is one VIR type.
- **Recursive object types.** An instance of an object type that reaches itself through its
  fields (`interface List<T> { tail?: List<T> }`) stays as it is: canonicalizing it would not
  terminate, and its shape cannot be written without naming it.
- **Nested null.** `T | null | null` is `T | null`: a generic `U | null` at `U = string | null`
  holds `null` as itself, as in JavaScript (`id<string | null>(null) === null` is `true`).
  `velt_vir` treats wrapping and unwrapping a payload that already is the option as the
  identity.
- **Generic unions.** `{ a: U | string }` at `U = i64` is the written `{ a: string | i64 }`, and
  `A | B` at `A = B = string` is `string`. Sema exports the concrete union of each member list
  (`Program::union_shapes`), and `velt_vir` remaps a generic union's variants by member type
  (`Cx::union_variant`).
- **Symbols.** `velt_vir`'s `type_key` spells anonymous object types structurally
  (`{ a: string }`), so a symbol does not depend on which definition represents a shape.

### Optional fields: `?` is a flag over the declared type

A field keeps its declared type and an optional flag, and both are part of an object type's
identity (`anon::ShapeField`). `a?: F` reads as `F | undefined` and `a?: F | null` as
`F | null | undefined`, as in TypeScript. Every optional field of an object type or field-only
interface has a **presence flag**, so an absent key and a key holding `undefined` stay apart as
in Node:

- a spread copies an optional field only while it is present, a present `undefined` or `null`
  included, so `update(u, { deletedAt: null })` clears `deletedAt` and `update(u, {})` keeps it;
- `JSON.stringify` and `console.log` leave out an absent field and print a present `null`;
  `JSON.parse` records whether the key was there;
- `{ a?: F }` and `{ a: F | undefined }` are different types with no implicit conversion; the
  mismatch suggests `{ ...x }`.

The flag is a `bool` stored after the fields of the VIR aggregate (`Cx::presence_slot`), so copy,
clone and equality glue carry it. A literal leaves a field absent with `Intrinsic::FieldAbsent`,
any write sets the flag, and spreads read it with `Intrinsic::FieldPresent`. A class's fields
always exist in its fixed layout: an optional class field starts as an own key holding
`undefined`, as in Node.

### Structural bounds

`T extends { name: string; id?: number }`, or a field-only interface bound, means that `T` has
at least these public fields, each assignable to the bound's type, a required field satisfying an
optional one, as TypeScript checks it. Object types, field-only interfaces, classes and structs
satisfy it, and so does a getter. Inside the body, `x.name` with `x: T` reads and writes the
field.

### Deferred forms

`TyKind::Op(TyOp, args)` is a type that cannot be reduced yet because an argument it inspects is
a type parameter or another deferred type (`crates/velt_sema/src/hir/ty_op.rs`):

| `TyOp` | Form |
|---|---|
| `Alias(n)` | a generic alias whose body (a conditional, mapped, indexed access or template literal type) needs its arguments |
| `Inline(n)` | a conditional, mapped, indexed access, `keyof` or utility type written in a generic definition outside an alias (`Partial<T>` in a signature), over the definition's type parameters (`crates/velt_sema/src/type_level/inline.rs`) |
| `Intersect` | `A & B` with a part that is a type parameter or deferred, reduced by `crate::intersections` |
| `Spread` | `{ ...t, $set }` with a part known only per instantiation |
| `Convert(n)` | never a value's type: the requirement that a conversion keeps one object type at every instantiation |

Everything else reduces where it is written: `Partial<{ a: U }>` is `{ a?: U }` inside `f<U>`.
`type_level::instances` carries each function's deferred types through its call sites (calls,
function values, `new`, generic methods called through a type parameter, dynamically dispatched
methods) to concrete arguments and reduces each one an instantiation reaches; lowering reads the
reductions from `hir::Program::type_ops` and evaluates no types itself.

The built-in operators `NonNullable`, `Awaited`, `Exclude`, `Extract` and the string
intrinsics (`Uppercase`, `Lowercase`, `Capitalize`, `Uncapitalize`) are in
`crates/velt_sema/src/type_level/builtins.rs`; mapped types in `type_level/mapped.rs`.

### Values of deferred types in a generic body

- **Member reads.** `p.name` with `p: Partial<T>`, `Pick<T, K>` or `Readonly<T>` and
  `T extends User` reads through `T`'s object bound: the value is read as an `any` and the
  member converted to the bound's field type
  (`crates/velt_sema/src/body/expr/generic_member.rs`). Members of an intersection are read
  through its parts.
- **`keyof T` and `T[K]`.** `keyof` is a prefix in type position. `x[k]` with `k: K` and
  `K extends keyof T` has type `T[K]` and is an `Intrinsic::KeyRead`; at every instantiation `K`
  must be keys of `T`'s object type, and lowering reads that field, or switches on the key for a
  union of keys (`crates/velt_sema/src/body/expr/key_index.rs`). Inference keeps literal keys:
  `pluck(users, "name")` infers `K = "name"` and returns `string[]`.
- **Writes through a key.** `x[k] = v` is an `Intrinsic::KeyWrite`. A value whose type is
  `T[K]` for a union `K` may belong to another key's field, so the write checks in its switch that
  the value fits the field and otherwise stops with a `TypeError`, where TypeScript would store a
  value of the wrong type (`crates/velt_vir/src/lower/spread_object.rs`).
- **Spreads.** `{ ...x, ...patch }` at a deferred type is `Intrinsic::SpreadObject` of type
  `TyOp::Spread`, expanded per instantiation by lowering into the merged object a concrete spread
  builds, presence tests included.
- **Conversions.** A conversion between deferred types (`T & U` as `T`) is the same value under
  the requirement `TyOp::Convert`: an error at an instantiation where the two are different object
  types, since a copy would lose the identity TypeScript keeps. Where the program cannot tell,
  conversions between different layouts are copies of the target's fields
  (`crate::object_copies`).
- **`{} as D`** with `D` a deferred object type (`Pick<T, K>` in the standard `pick` body) is
  allowed: it is a new record of `any` values, its keys are set through it (`r[k] = o[k]`), and
  converting it to `D` builds `D` from its entries at each instantiation, each value checked
  (`crates/velt_vir/src/lower/any_record.rs`). A missing required key is a `TypeError` that names
  it. Keys set in another order than `D`'s fields, or a key `D` lacks, are a `TypeError` too,
  since printing, serializing or listing the keys would differ from Node's object, which keeps
  the order the keys were set in.

### Errors at instantiation

Requirements are implicit (TypeScript does not ask for `T extends object`): an operand is an
object type, keys are fields, the parts of an intersection meet, a spread at a `T` needs an object
type. A failure is reported at the instantiation, naming the generic function and its type
arguments ("in `update` at this instantiation: `T = …`", or "reached through `f` …" for a
chain of calls). Termination of instantiation chains is checked by
`crates/velt_sema/src/instantiation_cycles.rs`, which rejects every growing edge in a strongly
connected component of the call graph.

## Runtime cost

- None for the types themselves. A read of a field through a key is a load, or one switch for a
  union of keys. Added work is what the source asks for: a presence test per optional field a
  spread reads, the check of a write through a key, and the copies `{ ...x }` makes. A presence
  flag costs one byte in its object.
- A member read through a type parameter's bound converts through `any` at run time.
- A program without deferred types pays nothing at compile time for them
  (`type_level::instances` returns at once).

## Compatibility

| TypeScript | Velt | Why |
|---|---|---|
| `T` assignable to `Partial<T>`, `Pick<T, K>`, `Omit<T, K>` (the same object) | in storage positions an error suggesting `{ ...x }` | different layouts; a conversion makes a new object |
| `{ a: F \| undefined }` assignable to `{ a?: F }`, and `{ a: F \| null }` to `{ a?: F \| null }` | an error suggesting `{ ...x }` | the optional field has a presence flag |
| `x[k] = v` with a union `K` stores any member | stops with a `TypeError` when the value does not fit the field | it would store a value of the wrong type |
| a spread of `Partial<T>` back into a `T` copies a present `undefined` into a required field | stops with a `TypeError` | the field cannot hold `undefined` |
| `{} as Pick<T, K>` filled in another key order | a `TypeError` | the object's key order would differ from Node's |

## Alternatives considered

**Re-checking generic bodies per instantiation** (C++ templates). It would accept every body
that is valid for its instantiations, but report errors inside library code at a user's call
site, make compile time grow with the number of instantiations, and give generic code no checked
meaning of its own.

## Not yet implemented

- **Writes to a named field of a deferred form**: `p.name = v` with `p: Partial<T>`,
  `Pick<T, K>`, `Omit<T, K>` or `Required<T>` is an error; only writes through a key
  (`r[k] = v`) work.
- **Compound assignment through a key** (`x[k] += v` with `K extends keyof T`).
- **Spreads of a value that may be missing**: `{ ...x }` with `x: T | undefined` or `T | null`
  in a generic body is an error ("cannot spread a value of type `T | undefined` into an
  object"); testing the value first works (`x === undefined ? {} : { ...x }`).
- **Implicit generic parameters** ([#672](https://github.com/velt-lang/velt/issues/672)): a
  parameter typed `Partial<User>`, `Pick<User, K>` or `Omit<User, K>` accepts a `User` as the same
  object, as TypeScript does, by making the parameter generic.
