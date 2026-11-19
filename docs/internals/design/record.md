# `Record<K, V>`

Status: implemented. Issue: [#17](https://github.com/velt-lang/velt/issues/17).

## Summary

`Record<K, V>` is TypeScript's dictionary type with TypeScript's object syntax: `r[k]`,
`r.name`, `r[k] = v`, `delete r[k]`, `k in r`, object literals and `Object.keys`. It is a prelude
class over the same insertion-ordered hash table as `Map`, so a read or write is one hash probe,
and it reads and writes JSON objects with arbitrary keys.

## Motivation

TypeScript code uses `Record<string, T>` for dictionaries, mostly for JSON-shaped data: headers,
per-locale strings, counts by name. Porting to `Map<K, V>` would mean rewriting every `r[k]`,
`r[k] = v`, `Object.keys(r)` and `{ a: 1 }` literal into method calls and `new Map()`:

```ts
type Config = { env: Record<string, string>; limits: Record<"cpu" | "mem", i64> };
const c = JSON.parse<Config>(`{"env":{"HOME":"/root"},"limits":{"cpu":2,"mem":512}}`);
const home = c.env["HOME"] ?? "/";
c.env["PATH"] = "/bin";
for (const k of Object.keys(c.env)) {
  console.log(k, c.env[k]);
}
const l: Record<"cpu" | "mem", i64> = { cpu: 2, mem: 512 };
console.log(home, l.cpu + c.limits.mem);
```

## Design

**Key types.** `K` is `string` (an *open* record), a union of string literal types or a string
enum (a *closed* record, which always has every key). A written `number` or `string | number`
key becomes `string`, as JavaScript makes a number key a string. Any other `K` is an error that
suggests `Map<K, V>`. A written key type is checked where the type is resolved; a type parameter
used as a key is checked at each instantiation, where it meets a concrete type
(`crates/velt_sema/src/record_keys.rs`).

**A reference type.** Like JavaScript objects and `Map`, assigning a record shares it (both
names see the same entries, and `==` compares identity), and `clone()` copies it. `deepEqual`
compares two records by their entries in any key order.

**Indexing.**

| Expression | Open record (`K = string`) | Closed record |
|---|---|---|
| `r[k]` | `V`, as in TypeScript; a missing key gives `undefined` (a `V \| undefined` underneath), tested by `=== undefined`, `??` and `?.`; a use as a `V` (a member read, a call, an index) throws Node's `TypeError` | `V`: every key is present |
| `r.name` (an identifier key) | same as `r["name"]` | same, and only for members of `K` (a typo is an error); for a string enum, the member whose value is `"name"` |
| `r[k] = v` | insert or replace | replace |
| `r[k] += 1`, `r[k]++` | as in JavaScript for a `number` (a missing key gives `NaN`) or a `string` (`"undefined"`); an error for any other `V` or a `V` that may be `null`, with a fix that says what a missing key starts from (`r[k] = (r[k] ?? 0) + 1` for an integer); `r[k] ??= v`, `\|\|=` and `&&=` are allowed | replace |
| `k in r` | presence test at run time | true for every key of `K` |
| `delete r[k]` | removes the key | an error: a closed record always has every key |

**Construction.** An object literal can be used wherever a `Record` is expected:

- `const r: Record<string, i64> = {}` is empty;
- `{ a: 1, ...other }` is allowed;
- for a closed record the literal lists every key, so `r[k]` is total.

A struct or object type does not convert to a record implicitly, except to a record of `any` or
`unknown` values: an object of an object literal type, or a record of other values, converts to
one as a view of the one object (`crates/velt_vir/src/lower/record_view.rs`), as
`x as Record<string, any>` of an `any` holding an object does. Reads see the object's current
entries and writes reach it; adding a key the object type has no field for throws a `TypeError`
where Node adds it.

**Key order.** Entries follow JavaScript's order: keys that are array indices (canonical `"0"`
to `"4294967294"`) first, ascending, then the others in insertion order. This applies to
iteration, printing and JSON.

**Iteration and helpers.** These follow TypeScript:

- `Object.keys(r)`, `Object.values(r)` and `Object.entries(r)` return arrays, and
  `for (const [k, v] of Object.entries(r))` iterates. A record itself is not iterable.
- `Object.values` and `Object.entries` need one value type, so they take a `Record`; given an
  object literal, they read it as a `Record<string, V>` whose values must share one type.
- `Object.keys(x)` returns a `string[]` and accepts any object, as in TypeScript: a record, an
  object literal or object type, a struct, or a class instance (its fields in declaration
  order, base class first; for a class with subclasses, the fields of the instance's dynamic
  class). An optional field of an object type is listed while it is present.

A record has no methods of its own, because in TypeScript `r.size` reads the key `"size"`.
`delete` applies only to records.

**Generic keys.** Where `K` is a type parameter, the record may be closed, so generic code reads
`r[k]` as an open record does and has no empty construction (`{}`; a literal with a spread is
fine) and no `delete`.

**JSON.** A record reads from and writes to a JSON object, in JavaScript's key order. For a
closed record, a missing key is a decoding error and an unknown key follows the decoder's
unknown-key option. Values use the usual rules.

## Compilation

`Record<K, V>` is a prelude class (`std/prelude/record.velt`) over `Map`'s table; the
`Object` helpers are in `std/prelude/object.velt`. Sema turns `r[k]`, `r.name`, `r[k] = v`,
`delete r[k]`, `k in r` and object literals typed as a record into calls of keyed methods
(`crates/velt_sema/src/body/expr/record.rs`, `record_literal.rs`, `record_compound.rs`,
`in_keys.rs`): one for open reads, which may find nothing, and one for closed reads, which
cannot. Those methods are keyed by internal symbols, so programs cannot call them. A key that is
an array index marks the record unordered; the readers then sort the entries they read without
changing the record, and a record without such keys pays one byte test per new key.

## Runtime cost

A read or write is one hash probe, the cost of `Map.get` / `Map.set`.

## Diagnostics

- `` `f64` cannot be a `Record` key ``, with the note "record keys are `string`, a union of
  string literals or a string enum; use `Map<K, V>` for other keys"; at a generic call, with the
  note ``required because `dec` uses it as a `Record` key``.
- `` `"cpu" | "mem"` has no key "disk" `` on a closed-record access or literal.
- ``missing key "mem" in a `Record<"cpu" | "mem", ...>` literal``.
- ``cannot iterate over a `Record<string, i64>` with `for...of` ``, with the fix
  `Object.keys(r)` / `Object.entries(r)`.
- ``cannot `delete` from a `Record<"a" | "b", ...>`: it always has every key``; ``cannot
  `delete` from a value of type `T` `` for anything that is not a record.
- ``cannot `delete` from a `Record<K, ...>`: the key type `K` is a type parameter``, with a note
  to build or change the record where its key type is known.
- `` `Record` cannot be extended ``, with the note to use composition (a `Record` field): a
  subclass's constructor would leave a closed record without its keys.

## Limitations

- A closed record is a hash table, not a fixed layout.
