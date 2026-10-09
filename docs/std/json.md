# velt:json

`import { Value } from "velt:json"`. `JSON` itself is in the prelude. This module only exports
`Value`, the dynamic JSON value (an alias of the prelude's `JsonValue`).

- `JSON.stringify<T>(x)`, `JSON.parse<T>(text, options?): T` (throws `JsonError`, e.g.
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
- **Private fields:** `JSON.stringify` writes a class's `private` fields, as Node does, and
  skips ES private fields (`#x`), as JavaScript does. `JSON.parse<T>` cannot build a class or
  struct with a `private` or `#` field (own or inherited), wherever it appears in `T`: decoding
  fills fields without running the constructor, so it could not initialize them; the error
  names the field. A type with a private field declared by a std type has no JSON form in either
  direction: std keeps runtime handles there (`#` fields of classes such as `BigInt`, `RegExp`,
  `JsonValue`, HTTP requests and responses, SQLite statements, `AbortSignal` and `TaskScope`;
  `private` fields of structs such as `Mutex`, sockets, files and database clients), also when
  a user class extends one. So JSON can never carry or forge a handle. To
  send such a value, convert it to a type with public fields first (`n.toString()` for a
  `BigInt`, or an object literal of the data you need). A class value is written as its
  dynamic class (a subclass's fields too), unless that class holds std private state: then as
  its static class.

```ts
class Account {
  name: string = "ada";
  private secret: string = "pw";
  #pin: string = "1234";
}

console.log(JSON.stringify(new Account())); // {"name":"ada","secret":"pw"}
```

```ts error
class Account {
  name: string = "ada";
  private secret: string = "pw";
}

// error: cannot convert to or from JSON: `Account` has a private field `secret`, which decoding cannot set
const a = JSON.parse<Account>('{"name":"ada","secret":"pw"}');
```

- **`toJSON()`:** `JSON.stringify` writes a class with a `toJSON()` method (its own or
  inherited, with no parameters) as what the method returns, as JavaScript does: a `Date` (and
  a subclass of `Date`) as its ISO string (`null` when invalid), a `URL` as its `href`, a user
  class as whatever its `toJSON()` makes. This also holds for a class that has no JSON form
  otherwise (std private state). `JSON.parse<T>` does not use it: decoding such a class still
  needs the rules above.

```ts
class Stamp extends Date {}

class Money {
  cents: number;
  constructor(cents: number) {
    this.cents = cents;
  }
  toJSON(): string {
    return `${this.cents / 100} EUR`;
  }
}

// {"at":"1970-01-01T00:00:00.000Z","price":"12.5 EUR"}
console.log(JSON.stringify({ at: new Stamp(0), price: new Money(1250) }));
```

- **Private and protected constructors:** `JSON.parse<T>` (and `v.as<T>()`) cannot decode a
  class whose constructor is `private` or `protected`, wherever it appears in `T` (a field, an
  array element, a union member, a `Map` or `Record` value): decoding fills the fields without
  running a constructor, so it would bypass the class's factories and their checks. Decode a
  plain object type and call the factory instead. `JSON.stringify` writes such a class as
  usual, and `Value` (whose constructor is private too) decodes: the runtime makes it.

```ts
class Money {
  private constructor(readonly cents: i64) {}

  static of(cents: i64): Money {
    if (cents < 0) throw new Error("negative amount");
    return new Money(cents);
  }
}

type MoneyJson = { cents: i64 };

const text = JSON.stringify(Money.of(250)); // {"cents":250}
// JSON.parse<Money>(text) is an error: `JSON.parse` cannot create a `Money`: its constructor is private
const m = Money.of(JSON.parse<MoneyJson>(text).cents);
console.log(text, m.cents); // {"cents":250} 250
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
- String escapes `\uXXXX` decode to UTF-16 code units, as in JavaScript: an escaped pair is one
  character, and a lone surrogate (a high one without a low one after it, or a low one alone)
  stays a lone surrogate, in `JSON.parse` and `JSON.parseValue` alike
  (`JSON.parse<string>('"\\ud800"').length` is 1). `JSON.stringify` writes it back as `\ud800`;
  printing or writing the string elsewhere gives U+FFFD (`�`).
- Syntax errors read the same from `JSON.parse<T>` and `JSON.parseValue`:
  `invalid JSON at $.items[2]: unexpected character '}' (byte 41)`. The offset counts bytes of
  the input's UTF-8, which is where an editor or `Buffer`-level tool finds it (not a string
  position: for ASCII input the two agree).
  In every message, a path of more than 20 segments keeps its first and last 10 with `…` between
  (`expected string at $.kids[0].kids[0].kids[0].kids[0].kids[0]…[0].kids[0].kids[0].kids[0].kids[0].name`);
  the byte offset still points at the exact place.
- `JSON.parse<T>` lets a `T | null` field (including `a?: T`) be missing, and then it is `null`;
  every other field is required. `JSON.stringify` omits an optional field (`a?: T`) of a class,
  object type or interface while it is absent, as JavaScript omits a missing property, and
  writes other `null`s (a `b: T | null` field, a `null` array element). An `a?: T` field
  without `null` in its type is absent exactly when it is `null` (TypeScript doesn't let it hold
  `null`). A field of an object type declared `a?: T | null` keeps the two apart, as JavaScript
  does: `JSON.parse` records whether the key was there, and `JSON.stringify` writes a present
  `null` (`"a":null`) and leaves out an absent field. In a class, such a field is still left out
  while it is `null`.
- `Value`:
  - navigation: `get(key)`, `at(i)`, both returning `Value | null`; `has(key)` (the key is
    present, even with a `null` value: absent vs explicit `null` is only visible here, as
    `!v.has("a")` vs `v.get("a")?.isNull()`); `len()`; `keys()`
  - type tests: `isNull isBool isNumber isString isArray isObject`
  - conversions: `asNumber(): f64 | null`, `asBool()`, `asString()`
  - building: `JsonValue.object()`, `JsonValue.array()`, `JsonValue.of(x)` (a string, number,
    `bool` or `null`), `JsonValue.from(x)` (the JSON form of any
    value `JSON.stringify` accepts, at any depth), `JsonValue.parse(text, options?)` (same as
    `JSON.parseValue`)
  - editing: `set(key, v)` (an existing key keeps its position), `delete(key)`, `push(v)`,
    `setAt(i, v)`; each returns `false` when the value is not an object / array (or `i` is out
    of range). `delete` costs O(1) amortized wherever the key is; `get` and `len` stay O(1)
    after it, and so does `at` on an object emptied from either end. After deletes in the
    middle of a large object (more than 16 members), `at` on it costs O(log n): the first such
    `at` takes O(n) to index the remaining members, and later edits keep that index up to date
    in O(log n) each, until the object is compacted
  - `as<T>(options?)`: decode into a `T`, like `JSON.parse<T>`
  - `stringify()` (keys in insertion order); `clone()` is O(1)
  - `console.log(v)` prints the value the way node prints the parsed object
    (`{ a: 1, b: [ 2, 'x' ], c: null }`; a string prints raw as a `console.log` argument and
    quoted inside other values), broken across lines like node when it is long, with node's
    limits (`[Object]` / `[Array]` past two levels of nesting, the first 100 elements of an
    array and then `... n more items`), as Velt's other values are
  - a template string prints a `JsonValue` exactly as `console.log` does, not as JSON:
    `` `v = ${v}` `` is `v = { a: 1, b: [ 2, 'x' ], c: null }`, and a JSON string `"hi"`
    shows as `hi`. Call `stringify()` for the JSON text (`{"a":1,"b":[2,"x"],"c":null}`)

  A `JsonValue` has value semantics: an edit never shows through a clone, through the value it
  was `set` into, or through a child handle from `get`/`at`. The runtime copies a node another
  handle shares before changing it (copy-on-write, one node at a time). To change a nested
  value, edit the child and `set` it back. There is no `v[k] = x` syntax: use `set`. Only the
  runtime makes `JsonValue`s: the constructor is private, so `new JsonValue(…)` is an error
  (use the static methods above; a JSON `null` is made only with `JsonValue.of(null)`), and a
  class cannot `extends` `JsonValue` (hold one in a field instead).

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
  console.log(v); // { tags: [ 'a', 'b' ], n: 1.5 }
}
```
