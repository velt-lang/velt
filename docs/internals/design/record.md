# Design: `Record<K, V>`

Status: decided (issue #17). The open questions were settled as recommended: `r[k]` is `V | null`
for `string` keys, `delete r[k]` removes, and dot access works. `Map<string, V>` already reads and
writes JSON objects with arbitrary keys.

## Problem

TypeScript code uses `Record<string, T>` for dictionaries, mostly for JSON-shaped data: headers,
per-locale strings, counts by name. Velt has `Map<K, V>`, but porting means rewriting every
`r[k]`, `r[k] = v`, `Object.keys(r)` and `{ a: 1 }` literal to `get`/`set` calls and `new Map()`.

```ts ignore
type Config = { env: Record<string, string>; limits: Record<"cpu" | "mem", i64> };
const c = JSON.parse<Config>(text);
const home = c.env["HOME"] ?? "/";
c.env["PATH"] = "/bin";
for (const k of Object.keys(c.env)) { console.log(k, c.env[k]); }
const l: Record<"cpu" | "mem", i64> = { cpu: 2, mem: 512 };
```

## Proposal

**Type.** `Record<K, V>`, where `K` is `string`, a union of string literal types, or a string
enum. Any other `K` is an error that suggests `Map<K, V>`. A written key type is checked where
the type is resolved; a type parameter used as a key is checked at each instantiation, where it
meets a concrete type (sema `record_keys.rs`, like the JSON check). Like JS objects and `Map`, a record is a
reference type: assigning one shares it (both names see the same entries, and `==` compares
identity), and `clone()` copies it. `deepEqual` compares two records by their entries in any
key order (equality glue: `Map.__deepEquals`).

**Representation.** It is a prelude class over the same insertion-ordered hash table as `Map`. The
compiler sees it as an ordinary class; the new parts are the typing rules below. No new runtime is
needed.

**Indexing.**

| Expression | `K = string` | `K` a literal union / string enum |
|---|---|---|
| `r[k]` | `V \| null`: the key may be missing (TS `noUncheckedIndexedAccess`) | `V`: every key is always present |
| `r.name` (an identifier key) | same as `r["name"]` | same, and only for members of `K` (a typo is an error); for a string enum, the member whose value is `"name"` |
| `r[k] = v` | insert or replace | replace |
| `r[k] += 1`, `r[k]++` | an error: the key may be missing (fix-it `r[k] = (r[k] ?? 0) + 1`); `r[k] ??= v` sets a missing key | replace |
| `k in r` | presence test (needs the `in` operator from #18) | always true; a warning |

**Construction.** An object literal can be used wherever a `Record` is expected:

- `const r: Record<string, i64> = {}` is empty.
- `{ a: 1, ...other }` is allowed.
- With literal keys, the literal must list every key, so `r[k]` can be total.
- There is no implicit conversion from a struct or object type.

**Iteration and helpers.** These follow TypeScript, in insertion order:

- `Object.keys(r)`, `Object.values(r)` and `Object.entries(r)` return arrays.
- `for (const [k, v] of Object.entries(r))`. A record itself is not iterable (`for (const k of
  r)` is an error suggesting `Object.keys(r)` / `Object.entries(r)`).
- Given an object literal (`Object.values({ a: 1 })`), `Object.values` and `Object.entries`
  read it as a `Record<string, V>` with `V` the first value's type (widened), which every
  other value must have.
- `Object.keys(x)` returns a `string[]` and accepts any object, as in TypeScript (issue #230):
  a record, an object literal or object type, a struct, or a class instance (its fields in
  declaration order, base class first; not a class with subclasses, whose dynamic fields sema
  cannot know).

They are prelude functions generic over `Record`. A record has no methods of its own, because in
TypeScript `r.size` would read the key `"size"`.

**Removal.** `delete r[k]`, only on records with `string` keys. `delete` stays out of the language
for every other target.

**Generic keys.** Where `K` is a type parameter, the record may be closed, so generic code treats
it conservatively: `r[k]` is `V | null`, and there is no empty construction (`{}` or
`new Record<K, V>()`; a literal with a spread is fine) and no `delete`. The prelude's `__`
methods are internal: calling one outside the prelude is an error.

**JSON.**

- A record reads from and writes to a JSON object, in insertion order.
- With literal keys, a missing key is `expected field "cpu"` and an unknown key follows the
  unknown-key option (#20).
- Values use the usual rules.

## Cost

- A read or write is one hash probe, the same as `Map.get` / `Map.set`.
- A literal-key record could later become a struct, a fixed layout with no hashing. That is an
  optimisation, not part of this proposal.

## Diagnostics

- ``index `Record<string, V>` with a `string` (found `i64`)``.
- `` `"cpu" | "mem"` has no key "disk" `` on a literal-key access or literal.
- `missing key "mem" in a Record<"cpu" | "mem", i64> literal`.
- A `Record` with another key type gets the note `use Map<K, V> for keys that are not strings`.
- `` `f64` cannot be a `Record` key `` at a generic call, with the note ``required because `dec`
  uses it as a `Record` key``; also where a generic class is instantiated whose method uses
  the key and may be called through an interface or a base class.
- `` `Record` cannot be extended ``, with the note to use composition (a `Record` field): a
  subclass's constructor would leave a closed record without its keys.
- ``cannot `delete` from a `Record<K, ...>`: the key type `K` is a type parameter``.
- `` `__delete` is internal to `Record`: a record has no methods of its own ``, with the
  replacement (`delete r[k]`) as a note.

## Decisions

1. `r[k]` on a `string`-keyed record is `V | null` (sound); literal-key records give `V`.
2. `delete r[k]` removes a key (only on `string`-keyed records).
3. `r.name` is `r["name"]` on every record.
4. Compound assignment (`r[k] += 1`, `r[k]++`, …) on an open record is an error with the fix-it
   `r[k] = (r[k] ?? 0) + 1` (issue #230): reads stay honest, with no `NaN` from a missing key
   as in JS. Closed records keep it.
