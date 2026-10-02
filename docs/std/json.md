# velt:json

`import { Value } from "velt:json"`. `JSON` itself is in the prelude. This module only exports
`Value`, the dynamic JSON value (an alias of the prelude's `JsonValue`).

- `JSON.stringify<T>(x)`, `JSON.parse<T>(text): T` (throws `JsonError`, e.g.
  `expected string at $.name`), `JSON.parseValue(text, options?): Value`.
- `JSON.parse<T>` decodes numbers, `bool`, `string`, arrays, `T | null`, structs, classes, object
  literals and `Value` (any JSON value, kept as a tree). A tuple (`[string, f64]`) is an array
  of exactly its length. A `Map<string, V>` or a `Record<string, V>` is an object with any keys,
  written in insertion order. A record with literal keys
  (`Record<"cpu" | "mem", i64>`) needs every key and skips other members. Unlike JavaScript, which writes a `Map` as `{}`,
  Velt writes its entries. Maps with other key types, functions, interfaces, promises and
  `shared` values have no JSON form; using them is a compile error. Values from a fixed set are checked:
  literal types (`kind: "task"`), unions of literal types (`"low" | "normal" | "high"`), string
  enums (from their strings) and numeric enums (from their values). Anything else fails with
  the allowed values, e.g. `expected one of "low", "normal", "high" at $.tags[1]`.
- **Private fields:** a class or struct with a `private` field (own or inherited) has no JSON
  form, for `JSON.parse<T>` and `JSON.stringify` alike; the error names the field. std types
  keep their runtime handles that way (`BigInt`, `RegExp`, `Mutex`, sockets, files, HTTP,
  database clients…), so untrusted JSON can never produce one. To send such a value, convert it
  to a type with public fields first (`n.toString()` for a `BigInt`, or an object literal of the
  data you need). A value whose static class has a JSON form but whose dynamic class has
  private fields is written as its static class.

```ts error
class Account {
  name: string = "ada";
  private secret: string = "pw";
}

// error: cannot convert to or from JSON: `Account` has a private field `secret`, so it has no JSON form
const text = JSON.stringify(new Account());
```

- Unions decode when `JSON.parse` can tell the members apart from the JSON value:
  - by its kind: `string | i64 | bool | null`, an array, or an object;
  - literal and enum members by value, before a plain member of the same kind (`"auto" | f64`);
  - several object members by a discriminant, a field with a different literal type in each
    (`{ kind: "join"; ... } | { kind: "leave"; ... }`) found anywhere in the object, or else by
    a required field only one member has (each member's first such field). Then the first of
    these fields in the document decides: with `Circle = { r: f64 }` and
    `Rect = { w: f64; h: f64 }`, `{"w":3,"h":2,"r":1}` is a `Rect`, and its `r` is an unknown
    key (skipped, or an error with `unknownKeys: "reject"`). A discriminant avoids the
    question.

  An unknown tag fails with `expected one of "join", "leave" at $.kind`. A union with two
  number types, two array types, a `Map`/`Record` beside another object, object members
  without a discriminant or distinguishing field, or two literal or enum members with the same
  value (`E | "a"` where `E.A = "a"`, `1 | 1.0`) is a compile error that explains which
  members clash.
- `JSON.parse<T>(text, options)` takes optional `JsonParseOptions`:
  - `unknownKeys: "reject"` makes an object key the target type has no field for an error
    (`unknown field at $.extra`); the default `"ignore"` skips it.
  - `maxDepth: n` limits how deeply arrays and objects nest, counting from the top-level
    value (`JSON nested deeper than 64 levels at $.a (byte 812)`). The default is 128 (like
    Rust's serde_json), so a deeply nested document from an untrusted source fails with a
    `JsonError` instead of crashing. A typed decoder uses stack space for every level: raise
    the limit only as far as your data needs, since a limit in the thousands can overflow the
    stack on a deep enough document.

  `JSON.parseValue(text, options)` (and `JsonValue.parse`, `v.as<T>(options)`) take the same
  options and the same default depth limit; `unknownKeys` has no effect on a `Value`.

  Integers stay exact without an option: an integer field reads the digits exactly, also
  beyond 2^53 (up to the `i64` range). Every integer type reads through `i64`, so a `u64`
  field stops at `i64::MAX` (9223372036854775807): a larger number fails with
  `expected u64`. There is no `Date` type to decode dates into: dates stay strings.
- A key that appears twice in an object keeps its last value, in the position of its first
  occurrence, like JavaScript: in structs, classes and object literals, `Map`, `Record` (with
  any key type) and `Value`. Unlike JavaScript, every occurrence must still be valid for the
  type: `{"age":"x","age":2}` fails with `expected i64 at $.age`. So a repeated literal field or
  union discriminant with two different values always fails (`{"kind":"leave",...,
  "kind":"join"}` with `expected "leave" at $.kind`: the first occurrence picks the member,
  whose literal the second one does not match).
- String escapes `\uXXXX` decode surrogate pairs to one character. A lone surrogate (a high
  one without a low one after it, or a low one alone) cannot be stored in UTF-8, so it becomes
  U+FFFD (`�`), in `JSON.parse` and `JSON.parseValue` alike; JavaScript keeps it as a lone
  UTF-16 unit.
- Syntax errors read the same from `JSON.parse<T>` and `JSON.parseValue`:
  `invalid JSON at $.items[2]: unexpected character '}' (byte 41)`.
- `JSON.parse<T>` treats an absent key and an explicit `null` alike: a `T | null` field
  (including `a?: T`) may be missing and is then `null`; every other field is required.
  `JSON.stringify` omits a `null` optional class field (`a?: T`) and writes other `null`s.
- `Value`:
  - navigation: `get(key)`, `at(i)`, both returning `Value | null`; `has(key)` (the key is
    present, even with a `null` value: absent vs explicit `null` is only visible here, as
    `!v.has("a")` vs `v.get("a")?.isNull()`); `len()`; `keys()`
  - type tests: `isNull isBool isNumber isString isArray isObject`
  - conversions: `asNumber(): f64 | null`, `asBool()`, `asString()`
  - building: `JsonValue.object()`, `JsonValue.array()`, `JsonValue.of(x)` (a string, number,
    `bool` or `null`; `new JsonValue()` is `null`), `JsonValue.from(x)` (the JSON form of any
    value `JSON.stringify` accepts, at any depth), `JsonValue.parse(text, options?)` (same as
    `JSON.parseValue`)
  - editing: `set(key, v)` (an existing key keeps its position), `delete(key)`, `push(v)`,
    `setAt(i, v)`; each returns `false` when the value is not an object / array (or `i` is out
    of range)
  - `as<T>(options?)`: decode into a `T`, like `JSON.parse<T>`
  - `stringify()` (keys in insertion order); `clone()` is O(1)

  A `JsonValue` has value semantics: an edit never shows through a clone, through the value it
  was `set` into, or through a child handle from `get`/`at`. The runtime copies a node another
  handle shares before changing it (copy-on-write, one node at a time). To change a nested
  value, edit the child and `set` it back. There is no `v[k] = x` syntax: use `set`. A class
  cannot `extends` `JsonValue` (only the runtime makes its values); hold one in a field
  instead.

```ts
import { Value } from "velt:json";

struct User {
  name: string;
  age: i64;
}

function main() {
  const u = JSON.parse<User>("{\"name\":\"Ada\",\"age\":36}");
  console.log(u.name, JSON.stringify(u));
  const v: Value = JSON.parseValue("{\"tags\":[\"a\",\"b\"],\"n\":1.5}");
  const tags = v.get("tags");
  if (tags != null) {
    console.log(tags.len(), tags.at(1)?.asString(), v.get("n")?.asNumber(), v.keys());
  }
}
```
