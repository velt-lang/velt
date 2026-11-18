# Data models shared with TypeScript

Status: implemented. Issues: [#326](https://github.com/velt-lang/velt/issues/326), [#61](https://github.com/velt-lang/velt/issues/61).

## Summary

A Velt backend and a TypeScript frontend can share their model files: the same `models.ts`
compiles with `tsc` and with `velt`. Three features make typical model code work: an interface
that has only fields is an object type, object types may have `readonly` fields, and the utility
types `Partial`, `Required`, `Readonly`, `Pick` and `Omit` exist.

## Motivation

```ts
interface User {
  readonly id: number;
  name: string;
  email?: string;
}

type NewUser = Omit<User, "id">;
type UserPatch = Partial<NewUser>;

const u: User = { id: 1, name: "ann" };
const patch: UserPatch = { email: "ann@example.com" };
console.log(JSON.stringify(u), JSON.stringify(patch));
```

Interfaces with methods are nominal in Velt: a type satisfies one by declaring `implements`, and
an interface value is a fat pointer (data and vtable) whose field reads are getter calls. That is
right for behavior and dynamic dispatch, and wrong for the most common TypeScript interface: a
list of fields describing data (DTOs, props, configs, API payloads).

## Design

### A field-only interface is an object type

An interface is **field-only** when it declares at least one field (own or inherited), no
methods, getters, setters or computed members, and every interface it extends is field-only. An
empty interface stays nominal. A field-only interface is a **named** object type with its fields:
it is shown as `User` in messages and may refer to itself (`interface Tree { children: Tree[] }`),
which anonymous object types, interned by structure, cannot. It converts to and from object types
with the same fields and stays the same object.

- **Fields** come in declaration order, inherited ones first. Generic field-only interfaces are
  generic object types (`interface Page<T> { items: T[]; total: number }`), and an instance
  (`Pair<string, number>`) is the object type it spells out, as for generic type aliases
  ([deferred-types.md](deferred-types.md#canonical-instantiation)).
- **Everything object types do, it does:** object literals satisfy it structurally (fields by
  name, optional fields may be left out), field reads are loads, values are stored inline, and
  `JSON.stringify` / `JSON.parse<User>` work.
- **Recursion through a direct field** (`next?: Node`) works: lowering stores an object type that
  reaches itself through inline fields (nested objects, nullable values, tuples, union payloads)
  as a counted box in every instance (the `recursive` set in
  `crates/velt_vir/src/lower/boxing/mod.rs`), so the field holds a pointer. `JSON.stringify`
  panics on a cycle ("converting circular structure to JSON", where JavaScript throws).
- **Interfaces with methods are unchanged:** nominal, `implements`, vtables, generic bounds.

**Classes.** `class Admin implements User` is valid and checks that the class has the fields
(same names and types). An `Admin` instance is not a `User` value: `User` is data, and an
instance is shared by reference, so converting would copy, and later writes through the instance
would not show in the copy. The conversion is an error whose notes explain this and offer the
fix: build a `User` from the instance (`{ id: a.id, name: a.name, email: a.email }`), or give
`User` a method to make it an interface classes implement.

**Generic bounds.** `function byId<T extends HasId>(xs: T[], id: number)` with
`interface HasId { id: number }` is a common pattern. A field-only interface is valid as a bound
and is satisfied **structurally**: a type with public fields of those names and types (object
types, classes, structs) satisfies it. Sema synthesizes the getter implementation for that type
when the bound is first checked (`Ctx::field_only_impl`), the same one a class gets from
`implements`, so lowering is unchanged and monomorphization inlines the getter into a field
read. A class that declares `implements` keeps its own implementation.

**Wider values.** A value of an object type with more fields than `User`, or the same fields in
another order, converts by copying the fields `User` has; where the program could tell the copy
from the original (it writes a copied field, compares by identity, prints or serializes the
value), the conversion is an error with the fix to build the object explicitly
(`crates/velt_sema/src/object_copies/mod.rs`).

### `readonly` fields in object types

`readonly name: T` in an object type (and so in a field-only interface), as in TypeScript.

- **Assignment** to a readonly field is the error ``cannot assign to `id`: it is a readonly
  field``, the message class fields use.
- **Identity and conversion:** `{ readonly id: number }` and `{ id: number }` are different types
  while bodies are checked (the flag is part of the shape), but a value of one converts to the
  other for free, in both directions, as in TypeScript, where `readonly` does not affect
  assignability. The conversion keeps identity: it is an `Upcast`, and before lowering every
  readonly object type is replaced by its twin without `readonly` (`velt_sema::readonly`).
  Lowering sees one type per layout, and both views are the same object.
- **Accessors in object types** (`{ get v(): number; set v(x: number) }`): each is read as a
  field of the getter's type (else the setter's), `readonly` without a setter, so an object type
  written in place keeps one layout. `ast::ObjectTypeField::accessors` keeps them as written for
  `velt fmt`.

### Utility types

`Partial<T>`, `Required<T>`, `Readonly<T>`, `Pick<T, K>` and `Omit<T, K>` are built-in type
operators (`crates/velt_sema/src/utility_types.rs`). On a concrete type each gives a new object
type; on a type parameter they are deferred types, reduced per instantiation
([deferred-types.md](deferred-types.md)). The operand is an object type, a field-only interface,
or a class or struct (whose public fields are used); `Pick` and `Omit` of an interface with
methods give an interface with those members.

| Operator | Result |
|---|---|
| `Partial<T>` | every field optional (`a?: F`, read as `F \| undefined`) |
| `Required<T>` | every field required; a written `\| null` stays (`Required<{ a?: string \| null }>` is `{ a: string \| null }`) |
| `Readonly<T>` | every field `readonly` |
| `Pick<T, K>` | only the fields named in `K`, in `T`'s order |
| `Omit<T, K>` | every field except those named in `K` |

- **`K`** is a string literal type or a union of them (`"id" | "email"`), also through an alias;
  a number literal names the property it spells. In `Pick`, a name that is not a field of `T` is
  an error (TypeScript's TS2344). In `Omit` it is a warning: TypeScript accepts any key there,
  and generic aliases rely on it (`type WithoutChildren<P> = Omit<P, "children">` used on a type
  without `children`). `Omit<T, never>` is `T`.
- **Results are ordinary object types:** they intern like any other, so `Pick<User, "name">`
  and `{ name: string }` are the same type, and they serialize to JSON.
- **Name lookup:** a user type named `Partial` (or one of the others) wins, as for every
  built-in.
- **Order:** an operator in a field type shapes the type it reads first (and the interfaces it
  extends or its base classes), so declaration order does not matter. A type that needs its own
  fields through an operator, directly or through other types
  (`interface Node { patches: Partial<Node>[] }`), is an error, where TypeScript accepts it.
- **Generic aliases** are expanded at each use, so an operator in an alias body reads the
  alias's type arguments at that use.

## Compilation

- **Parser:** `readonly` before a field name in object types, with the lookahead class members
  use, so a field named `readonly` still parses (`ast::ObjectTypeField::readonly`).
- **Sema:** anonymous object definitions carry `readonly` and optional flags in their fields and
  in the interning key; a coercion between shapes that differ only in `readonly` is an `Upcast`,
  and a pass after the program is checked erases `readonly` from every type
  (`crates/velt_sema/src/readonly.rs`). Interface collection marks field-only interfaces and
  gives each a synthesized object type definition
  (`crates/velt_sema/src/collect/field_only.rs`); as a bound, in `extends` and in `implements`,
  the name still means the interface. `implements` of a field-only interface checks fields.
- **Lowering:** no new runtime representation. Field-only interfaces use no vtables.

## Diagnostics

- ``cannot assign to `id`: it is a readonly field``.
- A class instance where a field-only interface is expected: "mismatched types", with the notes
  `` `User` has only fields, so it is a data type, like `type User = { … }`: a class instance is
  shared by reference and is not one `` and the fix-it above.
- `` `User` has no field `emial` (in `Pick`) ``, with "did you mean"; in `Omit` the same text is a
  warning, noting that there is nothing to omit.
- `` `Pick` takes the field names as a string literal type; found `string` ``.
- `` `Partial<Node>` needs the fields of `Node` while they are being resolved ``.
- `` `Partial` needs an object type; found `string[]` ``.

## Compatibility

- `Required` keeps a written `| null`, as TypeScript keeps it.
- A class instance does not convert to a field-only interface (TypeScript allows it).
- A type that needs its own fields through a utility type is an error.
- Utility types on type parameters, and `keyof`, are in [deferred-types.md](deferred-types.md).
