# The prelude

The prelude is the part of the standard library that every module sees without an import. It
lives in `std/prelude/*.vlt`; some of it (arrays' `push`/`pop`, `length`, `clone`, `spawn`,
`shared`) is implemented by the compiler.

## Strings

`string` is an immutable UTF-8 value ([Types](../reference/types.md#strings)). Positions are
byte offsets; a negative position counts from the end, as in JS.

| Method | Notes |
|---|---|
| `length` | byte length (`usize`) |
| `slice(start = 0, end?)`, `substring(start, end?)` | |
| `indexOf(s, from = 0)`, `lastIndexOf(s, from?)`, `includes(s)` | `-1` when absent |
| `startsWith(s)`, `endsWith(s)` | |
| `split(sep): string[]` | |
| `trim()`, `trimStart()`, `trimEnd()` | |
| `toUpperCase()`, `toLowerCase()` | |
| `replace(from, to)`, `replaceAll(from, to)` | plain text; for patterns use [`velt:regex`](regex.md) |
| `repeat(n)`, `padStart(n, fill = " ")`, `padEnd(n, fill = " ")` | |
| `charCodeAt(i = 0)` | the byte at `i` |

Conversions: `String.fromCharCode(code)`, `parseInt(s, radix = 0)` and `parseFloat(s)` (both
return `f64`, `NaN` on failure), `Number(s)`.

## Numbers

- `NaN`, `Infinity`, `isNaN(x)`, `isFinite(x)`.
- `x.toFixed(digits = 0)` on `f64`, rounded like JS.
- `Math`: `PI`, `E`, `sqrt floor ceil round trunc abs sign pow hypot`, `max(a, b)` and
  `min(a, b)` (two arguments). On integer operands, `Math.trunc(a / b)` is integer division.
- Every number type implements `Comparable` ([Comparable](../reference/classes.md#comparable)).

## Arrays

`T[]` is a growable array ([Types](../reference/types.md#objects-arrays-tuples-and-maps)).
Callback methods rethrow what their callback throws.

| Method | Notes |
|---|---|
| `length`, `push(x)`, `pop(): T \| null` | built in |
| `at(i): T \| null` | a negative `i` counts from the end |
| `forEach`, `map`, `filter`, `reduce(f, init)` | |
| `find`, `findIndex`, `findLast`, `findLastIndex`, `some`, `every` | |
| `indexOf`, `lastIndexOf`, `includes` | structural equality, so `NaN` is never found |
| `slice(start = 0, end?)`, `concat(other)`, `reverse()`, `fill(v, start?, end?)` | |
| `isEmpty()`, `entries(): [usize, T][]` | |
| `join(sep = ",")` | any element type, formatted like `${x}` |
| `sort()`, `sort(cmp)` | `sort()` on numbers, strings and `Comparable` elements (unstable, pdqsort); `sort(cmp)` is stable on any element type |
| `new Array<T>(n).fill(v)`, `Array.from({ length: n }, (_, i) => f(i))` | `n` elements in one allocation |

Byte arrays are plain `u8[]` with faster versions of `indexOf`, `lastIndexOf`, `includes`,
`fill`, plus `set(src, offset)` and `copyWithin(target, start, end)` like Node's `Buffer`.
`Buffer.alloc(n)` creates `n` zero bytes.

## Map

`Map<K, V>` is an insertion-ordered hash map. Keys are numbers, `bool`, `string`, class
instances (compared by identity), and structs, object types and tuples (compared by content).

| Member | Notes |
|---|---|
| `new Map<K, V>()`, `size`, `clear()` | |
| `set(k, v)`, `get(k): V \| null`, `has(k)`, `delete(k): bool` | `get` returns the stored value itself, as in JS |
| `upsert(k, init, (v) => v + 1)` | insert `init` or replace the value with the callback's result, in one lookup |
| `update(k, (v) => { … }): bool` | modify the stored value in place; `false` when `k` is absent |
| `getOrInsert(k, () => v)` | |
| `keys()`, `values()`, `entries()`, `forEach((v, k) => …)` | in insertion order |
| `for (const [k, v] of map)` | |

## Nullable values

On any `T | null`: `isNull()`, `unwrap()` (panics on `null`), `unwrapOr(fallback)`, and
`map(f)`. The value-extracting helpers return the payload itself (an object is shared, not
copied).

## JSON

`JSON.stringify<T>(x)`, `JSON.parse<T>(text)` (throws `JsonError`) and
`JSON.parseValue(text): JsonValue` ([`velt:json`](json.md)).

## Errors

- `class Error { message: string }`, the base class of thrown errors.
- `attempt(() => f())`: a throwing call as a value, `T | E`.
- `AggregateError`, thrown by `Promise.any` when every promise rejects.
- `assert(cond, msg?)`, `assertEq(a, b, msg?)` (compares with `deepEqual`), `panic(msg)`:
  panics, for bugs.
- `deepEqual(a, b): bool`: content comparison. Arrays, structs and object literals compare
  their contents recursively, class instances (`Map` included) by identity; `==` compares
  every object by identity.

## Async and concurrency

`sleep(ms)`, `yieldNow()`, `spawn(p)`, `Promise.all`, `Promise.race`, `Promise.allSettled`
(with `PromiseSettledResult<T, E>`), `Promise.any`, `shared(x)`, `Mutex<T>`,
`performance.now()` and `Date.now()` ([Async](../reference/async.md)).

```ts
const words = "the cat and the hat".split(" ");
const counts = new Map<string, i64>();
for (const w of words) {
  counts.upsert(w, 1, (n) => n + 1);
}
const top = counts.entries();
top.sort((a, b) => b[1] - a[1]);
console.log(top[0], words.at(-1), (2.0 / 3.0).toFixed(3), Math.max(3.0, 7.5));
// [ 'the', 2 ] hat 0.667 7.5
```
