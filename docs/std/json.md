# velt:json

`import { Value } from "velt:json"`. `JSON` itself is in the prelude. This module only exports
`Value`, the dynamic JSON value (an alias of the prelude's `JsonValue`).

- `JSON.stringify<T>(x)`, `JSON.parse<T>(text): T` (throws `JsonError`, e.g.
  `expected string at $.name`), `JSON.parseValue(text): Value`.
- `JSON.parse<T>` treats an absent key and an explicit `null` alike: a `T | null` field
  (including `a?: T`) may be missing and is then `null`; every other field is required.
  `JSON.stringify` omits a `null` optional class field (`a?: T`) and writes other `null`s.
- `Value`:
  - navigation: `get(key)`, `at(i)`, both returning `Value | null`; `has(key)` (the key is
    present, even with a `null` value: absent vs explicit `null` is only visible here, as
    `!v.has("a")` vs `v.get("a")?.isNull()`); `len()`; `keys()`
  - type tests: `isNull isBool isNumber isString isArray isObject`
  - conversions: `asNumber(): f64 | null`, `asBool()`, `asString()`
  - `stringify()`; `clone()` is O(1) because values are immutable

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
