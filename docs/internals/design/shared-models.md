# Design: data models shared with TypeScript

Status: implemented (issue #326, "Blockers for shared models"; answers #61). The maintainer's
decisions are in [Decisions](#decisions). All three steps are implemented: `readonly` fields in
object types, field-only interfaces, and utility types.

## Problem

A Velt backend and a TypeScript frontend should be able to share their model files: the same
`models.ts` compiles with `tsc` and `velt`. Three things in typical model code fail today:

```ts ignore
interface User {                       // nominal: a literal does not satisfy it
  readonly id: number;                 // `readonly` is a parse error in object types
  name: string;
  email?: string;
}

type NewUser = Omit<User, "id">;       // utility types don't exist
type UserPatch = Partial<NewUser>;

const u: User = { id: 1, name: "ann" };   // error: does not declare `implements User`
```

Interfaces are nominal in Velt: a type satisfies one only by declaring `implements`, and an
interface value is a fat pointer (data and vtable) whose field reads are getter calls. That is
right for interfaces with methods (behaviour, dynamic dispatch), and wrong for the most common
TypeScript interface: a list of fields describing data (DTOs, props, configs, API payloads).

(`JSON.stringify` of absent optional fields is left out here: it is a separate wire-format
decision.)

## Proposal

### 1. A field-only interface is an object type

An interface is **field-only** when it declares at least one field (own or inherited), no
methods, getters, setters or computed members, and every interface it extends is field-only. An
empty interface stays nominal. A field-only interface is an object type with its fields:

```ts ignore
interface User { readonly id: number; name: string; email?: string }
// behaves like
type User = { readonly id: number; name: string; email?: string };
```

It is a **named** object type (its own definition, shown as `User` in messages), not the
anonymous `{ … }` type itself: anonymous object types are interned by structure and can't refer
to themselves, while `interface Tree { children: Tree[] }` must. It converts to and from object
types with the same fields, and stays the same object: like `readonly` views, it is replaced by
the anonymous object type of its fields before lowering (`velt_sema::readonly`), and the
conversion is an `Upcast`. Two limits:

- **Recursion through a direct field** (`next?: Node`) has infinite size, as for a struct: an
  error, tracked in #376. Recursion through an array (`children: Node[]`) works.
- **A generic interface's instance** (`Pair<string, number>`) does not convert to the object
  type it spells out (`{ first: string; second: number }`): after erasure they are different
  definitions. The same holds for generic type aliases today; the error says so.

- **Fields** come in declaration order, inherited ones first (`interface B extends A` puts `A`'s
  fields first). Generic field-only interfaces are generic object types
  (`interface Page<T> { items: T[]; total: number }`).
- **Everything object types do, it does:** object literals satisfy it structurally (fields by
  name, optional fields may be left out), field reads are loads, values are stored inline, and
  `JSON.stringify` / `JSON.parse<User>` work. Interfaces had no JSON form before.
- **Interfaces with methods are unchanged:** nominal, `implements`, vtables, generic bounds.
- **Exactness stays:** like any object type, `User` accepts exactly its fields. A value of a
  wider object type is not a `User` (TypeScript allows that for non-literals); that is a separate
  question (open question 2).

**Classes.** `class Admin implements User` stays valid: it checks that the class has the fields
(same names and types). But an `Admin` instance is not a `User` value, because `User` is data,
copied by value, while an instance is shared by reference. Converting would copy, and later
writes through the instance would not show in the copy, a silent difference from TypeScript. So
the conversion is an error with a fix-it:

```text
error: an `Admin` instance is not a `User` value
  = note: `User` has only fields, so it is a data type, like `type User = { … }`
  = help: build one from the instance: `{ id: a.id, name: a.name, email: a.email }`, or give
          `User` a method to make it an interface classes implement
```

**Generic bounds.** `function byId<T extends HasId>(xs: T[], id: number)` with
`interface HasId { id: number }` is a common pattern. A field-only interface stays valid as a
bound, and is satisfied **structurally**: any non-generic type with public fields of those
names and types (object types, classes, structs) satisfies it. Sema synthesizes the getter impl
for that type when the bound is first checked, the same impl a class gets from `implements`, so
lowering is unchanged (and monomorphization inlines the getter into a field read). A class that
declares `implements` keeps its impl as before, generic types and generic field-only interfaces
are satisfied only through `implements`.

**Migration.** No field-only interface exists in std, the tests, examples or docs today, so
nothing in the repository changes meaning. User code that converted class instances to a
field-only interface gets the error above.

### 2. `readonly` fields in object types

`readonly name: T` in an object type (and so in a field-only interface), as in TypeScript.

- **Assignment** to a readonly field is an error: ``cannot assign to `id`: it is a readonly
  field``, the message class fields already use.
- **Identity and conversion:** `{ readonly id: number }` and `{ id: number }` are different types
  while bodies are checked (the flag is part of the shape), but a value of one converts to the
  other for free, in both directions, as in TypeScript (where `readonly` does not affect
  assignability). Object values are shared references, so the conversion must keep identity: it
  is an `Upcast` node, and before lowering every readonly object type is replaced by its twin
  without `readonly` (`velt_sema::readonly`). Lowering sees one type per layout, and both views
  are the same object.
- **Contract change:** `ast::ObjectTypeField` gains `readonly: bool`; `velt fmt` and `velt doc`
  print it.

### 3. Utility types

`Partial<T>`, `Required<T>`, `Readonly<T>`, `Pick<T, K>` and `Omit<T, K>`, as built-in type
operators. Each takes a concrete object type (including a field-only interface, or a class or
struct, whose public fields are used) and gives a new object type:

| Operator | Result |
|---|---|
| `Partial<T>` | every field optional (`T \| null`, may be left out) |
| `Required<T>` | every nullable field non-null (see below) |
| `Readonly<T>` | every field `readonly` |
| `Pick<T, K>` | only the fields named in `K`, in `T`'s order |
| `Omit<T, K>` | every field except those named in `K` |

- **`K`** is a string literal type or a union of them (`"id" | "email"`), also through an alias.
  In `Pick`, a name that is not a field of `T` is an error (TypeScript's TS2344). In `Omit` it is
  a warning with a "did you mean": TypeScript accepts any key there, and generic aliases rely
  on it (`type WithoutChildren<P> = Omit<P, "children">` used on a type without `children`).
  This was an error at first; #418 aligned it with TypeScript.
- **`Required`:** in Velt `a?: T` *is* `a: T | null` (documented: "`a?: T` is `T | null`
  everywhere"), so `Required` makes every nullable field non-null, also one written
  `a: T | null`. TypeScript only changes `?` fields. This is the documented difference.
- **Results are ordinary object types:** they intern like any other, so
  `Pick<User, "name">` and `{ name: string }` are the same type, and they serialize to JSON.
- **Name lookup:** a user type named `Partial` (or the others) wins, as for every built-in.
- **Order:** types are resolved in phases, and a field's type is resolved while declarations
  are still being shaped. An operator in a field type shapes the type it reads first (and the
  interfaces it extends or its base classes), so declaration order doesn't matter (#418). The
  one case left is a type that needs its own fields through an operator, directly or through
  other types (`interface Node { patches: Partial<Node>[] }`): an error, where TypeScript
  accepts it. Before interfaces are flattened, a field-only interface's inherited fields are
  read from its parents directly.
- **Generic aliases:** an alias is expanded at each use, so an operator in its body
  (`type WithoutChildren<P> = Omit<P, "children">`) reads the alias's type arguments at that
  use. Inside a generic function, where the argument is itself a type parameter, it is still
  the #350 error.
- **Not in this step:** applying an operator to a type parameter (`Partial<T>` inside a generic
  function). Velt resolves types eagerly and has no deferred type evaluation, so this is an
  error: ```Partial` needs a concrete object type; `T` is a type parameter``. It is the main
  follow-up, tracked in #350. Also not proposed: `keyof`, mapped types and `Record`-style index
  signatures beyond the existing `Record<K, V>`.

## Compiler changes

- **Parser:** `readonly` before a field name in object types (with the lookahead class members
  use, so a field named `readonly` still parses).
- **Sema:**
  - Anonymous object defs carry `readonly` (and keep using `T | null` for optional) in their
    fields and in the interning key; a coercion between shapes that differ only in `readonly`
    is an `Upcast`, and a pass after `finalize` erases `readonly` from every type in the
    program (implemented in step 1).
  - Interface collection marks field-only interfaces and resolves them to the object type of
    their fields instead of `TyKind::Dyn`. `implements` of a field-only interface checks fields
    only; class-to-interface conversion reports the error above.
  - Bounds: a field-only bound is checked structurally at instantiation, and field reads on a
    type parameter with such a bound become direct reads after monomorphization.
  - `resolve_builtin` gains the five operators, building results with `anon_type`.
- **Lowering:** no new runtime representation. Field-only interfaces stop using vtables.

## Diagnostics

- `an `Admin` instance is not a `User` value`, with the notes above.
- ``cannot assign to `id`: it is a readonly field`` (existing wording).
- ``` `User` has no field `emial` (in `Pick`) ``` (with "did you mean"); in `Omit`, the same text
  as a warning.
- ```Partial` needs a concrete object type; `T` is a type parameter``.
- ``a type argument of `Omit` must be a string literal or a union of them``.
- The existing ``does not declare `implements I` `` note stays for interfaces with methods, and
  gains a hint when the interface could be field-only: "this interface has methods, so it is
  nominal".

## Implementation order

1. `readonly` fields in object types.
2. Field-only interfaces as object types (structural literals, JSON, `implements` as a check,
   the conversion error, structural field-only bounds).
3. The utility types.

Each step updates `docs/reference/classes.md`, `docs/reference/types.md` and
`docs/book/ts-developers.md` (which lists utility types as unavailable).

## Decisions

1. **A class instance does not convert to a field-only interface:** an error with a fix-it, not
   a silent copy (`implements` still checks the fields).
2. **A key that is not a field is an error in `Pick` and a warning in `Omit`.** First decided as
   an error in both, to catch typos; #418 (after the #395 review) made `Omit` match TypeScript,
   which generic aliases need. `Required` clearing only `?` waits for `?:` as a flag (P2 in
   #395).
3. **Utility types on a type parameter come later,** with `keyof`, in their own design: #350.
4. **A field-only interface is a named object type** (so it can be recursive through arrays and
   messages name it), converting to and from object types of the same layout.
5. **Recursion through a direct field stays an error for now:** automatic boxing is #376.

## Open questions

1. Width subtyping: should a value of an object type with *more* fields convert to a field-only
   interface (TypeScript allows it for non-literals)? It needs a copy that drops fields; for
   object types (data, copied by value) that is not a silent difference.
