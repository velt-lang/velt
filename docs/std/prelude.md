# The prelude

The prelude is the part of the standard library that every module sees without an import. It
lives in `std/prelude/*.vlt`; some of it (arrays' `push`/`pop`, `length`, `clone`, `spawn`,
`shared`) is implemented by the compiler.

Node's web globals need no import either: `fetch`, `Request`, `Response` and `Headers`
([fetch](fetch.md)), `URL` and `URLSearchParams` ([velt:url](url.md)), and `AbortController` and
`AbortSignal` ([velt:task](task.md)), `Set` ([velt:collections/set](collections/set.md)),
`RegExp` ([velt:regex](regex.md); a regex literal counts as naming it), and `TextEncoder` and
`TextDecoder` ([velt:encoding](encoding.md)). They are loaded when a module names one and
doesn't bind that name itself. `structuredClone(x)` is a deep copy (`x.clone()`); a value that
JS can't clone (a function, at any depth) or would copy as a plain object (an instance of a
class of your own: write `x.clone()`) is a compile error. The builtin `process`
(`process.stdout.write(s)`, `process.env`, `process.argv`, `process.exit(code)`,
`process.memoryUsage()`; [velt:process](process.md)) needs no import either.

Every export and public member in `std/prelude` has a doc comment, which `velt doc --std` shows
under its signature. Where TypeScript has the same function, the text is adapted from the JSDoc
of TypeScript's `lib.*.d.ts` (Apache-2.0, see `NOTICE`) and edited for Velt's differences; this
page lists those differences.

## Strings

`string` is an immutable sequence of UTF-16 code units, as in JS
([Types](../reference/types.md#strings)): lengths and positions count code units (an emoji is
two), and a negative position counts from the end. `Buffer.byteLength(s)` is the size in UTF-8.

| Method | Notes |
|---|---|
| `length` | code units |
| `charAt(i = 0)`, `s[i]` | the code unit at `i` as a string, or `""` (JS: `s[i]` past the end is `undefined`) |
| `at(i): string \| null` | like `charAt`; a negative `i` counts from the end, `null` past the end |
| `slice(start = 0, end?)`, `substring(start, end?)` | |
| `indexOf(s, from = 0)`, `lastIndexOf(s, from?)`, `includes(s)` | `-1` when absent |
| `startsWith(s)`, `endsWith(s)` | |
| `split(sep): string[]` | `split("")` gives the code units (a pair splits into two halves); `for (const c of s)` and `[...s]` give characters |
| `trim()`, `trimStart()`, `trimEnd()` | |
| `toUpperCase()`, `toLowerCase()` | |
| `replace(from, to)`, `replaceAll(from, to)` | `from` is plain text, or a `RegExp` (as in `match`, `matchAll`, `search` and `split` with a regex: [`velt:regex`](regex.md#string-methods-with-a-regex)) |
| `repeat(n)`, `padStart(n, fill = " ")`, `padEnd(n, fill = " ")` | `repeat` panics on a negative `n` (JS's RangeError); the pads fill up to `n` code units |
| `charCodeAt(i = 0)` | the code unit at `i` (one half of a pair for an emoji); `-1` out of range (JS: `NaN`) |
| `localeCompare(t): i64` | -1, 0 or 1 in the CLDR root collation, like `new Intl.Collator("und").compare(s, t)` (`"a" < "A" < "b"`, `"e" < "é" < "f"`; Node's own `localeCompare` uses the host's locale). Exact for strings made of U+0020..U+024F, U+0370..U+04FF, U+1E00..U+1EFF, U+2000..U+206F and U+20A0..U+20CF (Latin with Vietnamese, Greek, Cyrillic, general punctuation, currency signs), except a few characters that stand for three or more (`¼`, `½`, `¾`, `ϗ`); approximate for everything else. No locale or options arguments |

`<`, `>` and `sort()` order strings by code units, as JS. A position between the two halves of
a pair is allowed everywhere: `"😀".slice(0, 1)` is a lone surrogate, which output writes as
U+FFFD, and the searches can match half of a pair (`"😀".indexOf(lo)` is 1 when `lo` is the low
half).

Conversions: `String.fromCharCode(...codes)` (any number of code units; a lone surrogate gives
a lone surrogate, a surrogate pair its character),
`parseInt(s, radix = 0)` and `parseFloat(s)` (both return `f64`, `NaN` on failure),
`Number(s)`.

## Numbers

- `NaN`, `Infinity`, `isNaN(x)`, `isFinite(x)`.
- `Number(s)` converts a string; `Number.isInteger(x)`, `Number.isNaN(x)`, `Number.isFinite(x)`,
  `Number.isSafeInteger(x)`, `Number.parseInt(s, radix = 0)`, `Number.parseFloat(s)` and the
  constants `Number.MAX_SAFE_INTEGER`, `MIN_SAFE_INTEGER`, `EPSILON`, `MAX_VALUE`, `MIN_VALUE`,
  `NaN`, `POSITIVE_INFINITY`, `NEGATIVE_INFINITY` are JS's, on `f64` (they live in the prelude
  class `NumberConstructor`, TypeScript's name for the type of `Number`).
- `x.toFixed(digits = 0)`, `x.toExponential(digits?)` and `x.toPrecision(precision?)` on `f64`,
  with JS's output and rounding (the nearest, an exact tie away from zero):

  ```ts
  console.log((123.456).toExponential(2), (0).toExponential()); // 1.23e+2 0e+0
  console.log((123.456).toPrecision(4), (0.000123).toPrecision(2)); // 123.5 0.00012
  ```

  A digit count is truncated as in JS (`toExponential(2.7)` is `toExponential(2)`); one out of
  range (`toFixed` and `toExponential`: 0 to 100, `toPrecision`: 1 to 100) panics like JS's
  `RangeError`.
- `Math`: `PI`, `E`, `sqrt floor ceil round trunc abs sign pow`, `max`, `min` and `hypot` (any
  number of values, spreads included: `Math.max(...xs)`), and `random()` (uniform in `[0, 1)`,
  not for secrets). On integer operands, `Math.trunc(a / b)` is integer division. `imul` (the
  32-bit wrapping product, one multiply instruction) and `clz32` (leading zero bits) take the low
  32 bits of their operands like JS. `umulh(a, b)` (not in JS) is the high 64 bits of the
  128-bit product of two `u64`s.
- Every number type implements `Comparable` ([Comparable](../reference/classes.md#comparable)).

## Arrays

`T[]` is a growable array ([Types](../reference/types.md#objects-arrays-tuples-and-maps)).
Callback methods rethrow what their callback throws. A callback may change the array through
another reference to it (`const ys = xs`, or an object holding it), and the element it received
stays valid however the array changes. As in JS, the methods read the length once at the start,
so elements pushed meanwhile are not visited. Elements removed meanwhile:
- `forEach`, `filter`, `reduce`, `some` and `every` skip them, as JS does;
- `find`, `findIndex`, `findLast` and `findLastIndex` skip them too, where JS calls the callback
  with `undefined` for each missing index (a `T` cannot be `undefined`);
- `map` panics with "the array shrank while `map` ran", where JS returns an array with holes;
- `filter` and `find` do not return the element the callback was given when the callback
  removed it, where JS does.

| Method | Notes |
|---|---|
| `length`, `push(x)`, `pop(): T \| null` | built in |
| `at(i): T \| null` | a negative `i` counts from the end |
| `forEach`, `map`, `filter`, `reduce(f, init)` | callbacks get `(x, i)` (`reduce`: `(acc, x, i)`) and may take fewer |
| `find`, `findIndex`, `findLast`, `findLastIndex`, `some`, `every` | likewise |
| `indexOf`, `lastIndexOf`, `includes` | compare with `==` (objects by identity, like JS's `===`), so `NaN` is never found (JS's `includes` finds it) |
| `slice(start = 0, end?)`, `concat(other)` | |
| `reverse()`, `fill(v, start?, end?)`, `sort()` | in place, returning nothing (JS returns the array: returning it would share it, which makes every array of its type reference counted). A member access chained on the call reads the changed array, as in JS: `xs.sort().join(",")`, `xs.reverse()[0]`, `s.split(",").sort().map(f)`. Such a chain changes the array, so it can't be an argument of a call that also reads the array (`console.log(xs.sort().join(), xs)` is an error, like any call that changes an argument another one reads): sort on its own line first. Any other use of the result (`return xs.sort()`, `const ys = xs.sort()`, `f(xs.sort())`) is an error: call the method on its own line and then use `xs`, or use `toSorted()` / `toReversed()` for a changed copy |
| `toSorted(cmp?)`, `toReversed()`, `toSpliced(start, deleteCount?, ...items)`, `with(i, v)` | ES2023's copying forms: a new array, the receiver unchanged (the elements themselves are shared, as in JS); `toSorted()` without a comparator orders like `sort()`; `with` panics on an index out of range (JS's RangeError) |
| `splice(start, deleteCount?, ...items): T[]` | removes and returns `deleteCount` elements (the rest when omitted) and inserts `items` there |
| `shift(): T \| null`, `unshift(...items): i64` | take the first element (`null` when empty) / insert `items` at the front and return the new length, like JS; both O(length), as in V8 for large arrays: a queue that takes from the front belongs in a `Deque` ([velt:collections/deque](collections/deque.md)) |
| `xs.length = n`, `truncate(n)` | drop the elements from `n` on, as in JS (`xs.length = 0` empties the array; `xs.length -= k` works too). `n` is a `number`: one that is not a whole number from 0 to 2^32 - 1 (negative, fractional, `NaN`) panics with `RangeError: Invalid array length`, the error JS throws. Setting a larger `length` panics too: JS would add empty slots, which Velt has no value for (push the elements, or build the array with `new Array<T>(n).fill(v)`); `truncate(n)` leaves the array unchanged instead |
| `flat()` | on `T[][]`: the inner elements, one level deep |
| `isEmpty()`, `entries(): [usize, T][]` | the index is a JS number, like `length` |
| `join(sep = ",")`, `toString()` | as JS: strings, numbers and booleans as `String(x)`, inner arrays joined with `","` at any depth, `null` elements as empty text, class instances through their `toString()` (a `Date` too), other objects as `[object Object]`, maps as `[object Map]` and sets as `[object Set]`; `toString()` is `join(",")`, what `${xs}` writes. Elements JS writes with a method Velt cannot call there (a struct's `toString()`, an `Error`, a `RegExp`) are a compile error: write `xs.map((x) => x.toString()).join(sep)` |
| `sort()`, `sort(cmp)` | `sort()` on `i64`, `i32`, `u64`, `usize`, `f64` and `string` elements, ascending with `NaN` last (unstable, pdqsort); other element types need a comparator; `sort(cmp)` is stable on any element type; the comparator returns a `number` (an `i64` on arrays of declared integers), and an arrow comparator may return an integer of any type; a comparator that reaches the array through an alias (`const ys = xs`) sees it unchanged while it runs when the elements are numbers, booleans or plain structs (as in JS), and empty for strings, arrays and objects |
| `new Array<T>(n).fill(v)`, `Array.from({ length: n }, (_, i) => f(i))` | `n` elements in one allocation |
| `Array.from(src)`, `Array.from(src, (v, i) => f(v, i))` | the values of anything `for...of` takes (an array, a string's characters, a map's entries, a generator, an iterable), mapped as they arrive |

Byte arrays are plain `u8[]` with faster versions of `indexOf`, `lastIndexOf`, `includes`,
`fill`, plus `set(src, offset)` and `copyWithin(target, start, end)` like Node's `Buffer`.
`Buffer.alloc(n)` creates `n` zero bytes. `Buffer.byteLength(s, encoding = "utf8")` is the
number of bytes `s` takes, as in Node: UTF-8 by default (O(1); a lone surrogate counts the 3
bytes of U+FFFD), two per code unit for `utf16le`/`ucs2`, one for `latin1`/`binary`/`ascii`,
the decoded size for `base64`/`base64url` and `hex`.

## Map

`Map<K, V>` is an insertion-ordered hash map. Keys are numbers, `bool`, `string`, class
instances, interface and function values (compared by identity, as `==` compares them), and
structs, object types, tuples, arrays, maps and records (compared by content, as `deepEqual`
compares them).

Float keys compare like JavaScript's (SameValueZero): `0` and `-0` are one key, and `NaN` is a
key that finds itself (`m.get(NaN)`). Floats inside content keys (arrays, tuples, object types)
compare the same way. `==` keeps IEEE comparison (`NaN == NaN` is `false`).

One difference from JavaScript's keys:

- **Content keys are hashed when they are inserted.** Changing an array, object, `Map` or
  `Record` after using it as a key leaves its entry unreachable: `get` finds it neither by the
  new content nor by the old (it still counts in `size` and shows up when iterating). Don't
  change a key while it is in a map; insert a copy, or delete the entry and insert it again.

```ts
const key: i64[] = [1];
const m = new Map<i64[], string>();
m.set(key, "one");
console.log(m.get([1])); // one
key.push(2);
console.log(m.get(key), m.get([1]), m.size); // null null 1
```

| Member | Notes |
|---|---|
| `new Map<K, V>()`, `new Map(entries: [K, V][])`, `new Map(iterable)`, `size`, `clear()` | `new Map(entries)` leaves `entries` as it is and shares their keys and values, like JS; a repeated key keeps its first position and its last value. Any iterable of `[K, V]` pairs (a generator, another map) works too |
| `set(k, v)`, `get(k): V \| null`, `has(k)`, `delete(k): bool` | `get` returns the stored value itself, as in JS, and `null` for a missing key (JS: `undefined`, so `console.log(m.get(k))` prints `null` where Node prints `undefined`) |
| `upsert(k, init, (v) => v + 1)` | insert `init` or replace the value with the callback's result, in one lookup |
| `update(k, (v) => { … }): bool` | modify the stored value in place; `false` when `k` is absent |
| `getOrInsert(k, () => v)` | |
| `keys()`, `values()`, `entries()`, `forEach((v, k) => …)` | in insertion order; the first three return arrays (JS: iterators). `forEach` is live, as in JS: it visits entries its callback adds and skips the ones it deletes |
| `[Symbol.iterator](): Iterator<[K, V]>` | a map is an `Iterable<[K, V]>`; the iterator visits the entries as of the call (JS's is a live view) |
| `for (const [k, v] of map)` | live, as in JS: so is `for...of` over `map.keys()`, `map.values()` and `map.entries()` when `map` is a variable, `this` or a field of one (any other map expression iterates the array of entries as of the loop's start). The loop walks the map object `map` named when it started, so assigning another map to `map` in the body does not change what it visits, as in JS |

A callback of `forEach`, `upsert`, `update` or `getOrInsert` may change the map through another
reference to it: the value the callback gets stays valid, `forEach` visits entries added
meanwhile, and `upsert` and `getOrInsert` store their result under the key even when the
callback deleted or added entries.

## Record

`Record<K, V>` is a dictionary written with TypeScript object syntax: `r[k]`, `r.name`,
`r[k] = v`, `delete r[k]` and object literals
([Reference](../reference/types.md#objects-arrays-tuples-and-maps)). `Object.keys(r)`,
`Object.values(r)` and `Object.entries(r)` return arrays in insertion order; `Object.keys`
returns a `string[]` and, as in TypeScript, also lists the fields of any object, struct or
class instance (except `#private` fields, as in JS).

```ts
class User {
  name = "a";
  private age = 3;
  static readonly limit = 9;
}
console.log(Object.keys({ id: 1, tag: "x" }), Object.keys(new User()));
// [ 'id', 'tag' ] [ 'name', 'age' ]
```

```ts
const env: Record<string, string> = { HOME: "/home/a" };
env["PATH"] = "/bin";
const limits: Record<"cpu" | "mem", i64> = { cpu: 2, mem: 512 };
limits.cpu += 1;
console.log(env.HOME ?? "/", Object.keys(env), limits);
// /home/a [ 'HOME', 'PATH' ] { cpu: 3, mem: 512 }
```

A key of a `Record<string, V>` may be missing, so counting needs a starting value:
`r[k] += 1` is an error there, and `??` supplies it. `r[k] ||= v` and `r[k] &&= v` are errors
too; `r[k] ??= v` sets the key only when it is missing.

```ts
const seen: Record<string, i64> = {};
for (const w of ["a", "b", "a"]) {
  seen[w] = (seen[w] ?? 0) + 1;
}
console.log(seen); // { a: 2, b: 1 }
```

## Nullable values

On any `T | null`: `isNull()`, `unwrap()` (panics on `null`), `unwrapOr(fallback)`, and
`map(f)`. The value-extracting helpers return the payload itself (an object is shared, not
copied).

## Iteration

The iteration protocol behind `for...of` and `for await` ([Control flow](../reference/control-flow.md#iterables)):

| Declaration | Notes |
|---|---|
| `type IteratorResult<T> = { value: T; done: false } \| { done: true }` | narrows on `r.done`; a done result has no `value` (unnarrowed, `r.value` is `T \| null`) |
| `interface Iterator<T, E = never>` | `next(): IteratorResult<T> throws E`; `return(): IteratorResult<T>` (default: returns `{ done: true }`) runs when a loop leaves early |
| `interface Iterable<T, E = never>` | `[Symbol.iterator](): Iterator<T, E>`; what `for...of` iterates |
| `class Generator<T, E = never>` | what calling a [generator](../reference/functions.md#generators) (`function*`) creates: `implements Iterator<T, E>, Iterable<T, E>` (`[Symbol.iterator]()` returns itself); `return()` (which returns `{ done: true }`) and `[Symbol.dispose]()` close it. Only generator calls create one (`new Generator` is an error) |
| `interface AsyncIterator<T, E = never>` | `next(): Promise<IteratorResult<T>, E>`; `async return(): Promise<IteratorResult<T>>` (default: resolves to `{ done: true }`) |
| `interface AsyncIterable<T, E = never>` | `[Symbol.asyncIterator](): AsyncIterator<T, E>`; what [`for await`](../reference/control-flow.md#for-await) iterates |
| `class AsyncGenerator<T, E = never>` | what calling an [async generator](../reference/functions.md#async-generators) (`async function*`) creates: `implements AsyncIterator<T, E>, AsyncIterable<T, E>`; `return()` and `[Symbol.asyncDispose]()` close it (awaiting its cleanup), `[Symbol.dispose]()` closes it without awaiting. Overlapping `next()` / `return()` calls are queued and served in call order, like JS. Only async generator calls create one |
| `interface IterableIterator<T, E = never>`, `interface IteratorObject<T, E = never>` | TS's iterators that are also iterable: `extends Iterator<T, E>, Iterable<T, E>` (`[Symbol.iterator]()` returns the iterator itself, as an `Iterator<T, E>`). `Generator` implements both; a value of either converts to an `Iterable<T, E>` |
| `interface AsyncIterableIterator<T, E = never>` | `extends AsyncIterator<T, E>, AsyncIterable<T, E>`; `AsyncGenerator` implements it, and a value converts to an `AsyncIterable<T, E>` |
| `[Symbol.iterator](): Iterator<T>` on `T[]`, `string` (`Iterator<string>`), `Map<K, V>` (`Iterator<[K, V]>`) | makes them `Iterable`: they convert to `Iterable<T>` values and satisfy `Iterable<T>` bounds. An array's iterator is a live view (JS's: it reads the length at each step); a string's yields characters (code points); a map's iterates the entries as of the call (`entries()`) |
| `class ArrayIterator<T>`, `class StringIterator` | the iterators of arrays and strings (TS's names): `implements IterableIterator<T>, IteratorObject<T>` |
| `class __IterableObject<T, E = never>`, `class __AsyncIterableObject<T, E = never>` | an [iterable object literal](../reference/types.md#iterable-object-literals) (`{ *[Symbol.iterator]() { ... } }`): `implements Iterable<T, E>` (`AsyncIterable<T, E>`) by calling the method it holds |

An `extend` block defining `[Symbol.iterator](): Iterator<T, E>` (or
`[Symbol.asyncIterator](): AsyncIterator<T, E>`) makes its type an `Iterable<T, E>` (or
`AsyncIterable<T, E>`) the way the prelude does for arrays and strings, as `compareTo` makes it
`Comparable`.

## JSON

`JSON.stringify<T>(x)`, `JSON.parse<T>(text, options?)` (throws `JsonError`) and
`JSON.parseValue(text, options?): JsonValue` ([`velt:json`](json.md)); arrays and objects may
nest 128 levels deep unless `options.maxDepth` says otherwise.

## Errors

- `class Error { message: string }`, the base class of thrown errors.
- `attempt(() => f())`: a throwing call as a value, `T | E`.
- `AggregateError`, thrown by `Promise.any` when every promise rejects.
- `assert(cond, msg?)`, `assertEq(a, b, msg?)` (compares with `deepEqual`), `panic(msg)`:
  panics, for bugs. `assertThrows(() => f(), msg?)` returns the error `f` throws and panics if
  it returns normally.
- `deepEqual(a, b): bool`: content comparison. Arrays, structs and object literals compare
  their contents recursively. A `Map` or `Record` equals another with the same keys, each with
  a deeply equal value, in any order (a key is matched as `get` matches it). Other class
  instances compare by identity; `==` compares every object by identity. This is Node's
  `util.isDeepStrictEqual` except for `-0`: floats compare as map keys do (SameValueZero), so
  `deepEqual([NaN], [NaN])` is `true` like Node, and `deepEqual([0], [-0])` is `true` (Node
  says `false`).

```ts
const a: Record<string, i64[]> = { x: [1], y: [2] };
const b: Record<string, i64[]> = { y: [2], x: [1] };
console.log(deepEqual(a, b), a == b); // true false
```

## Date

`Date` is JavaScript's: an instant in milliseconds since the epoch (`NaN` when invalid), with
months 0-11, local-time getters and setters (the OS time zone, DST-aware) and their `UTC`
variants. It is built on [`velt:datetime`](datetime.md), whose `DateTime` (UTC-first, months
1-12) is the better choice for new code.

- `new Date()`, `new Date(ms)`, `new Date(string)`, `new Date(date)`,
  `new Date(year, month, day?, hours?, minutes?, seconds?, ms?)` (local time, rolling over).
- `Date.now()`, `Date.parse(s)` (ISO 8601 and HTTP dates; a date alone is UTC, a date-time
  without an offset local time, as in JS), `Date.UTC(year, month, …)`.
- `getTime()`, `valueOf()`, `getTimezoneOffset()`; `getFullYear getMonth getDate getDay
  getHours getMinutes getSeconds getMilliseconds` and the `getUTC…` ones; the matching `set…`
  and `setUTC…` setters (with JS's optional extra fields), and `setTime`.
- `toISOString()` (an invalid date panics), `toJSON()` (the ISO string, or `null` when
  invalid; `JSON.stringify` writes a date, or a subclass of `Date`, through it, as Node
  does), `toUTCString()`, `toString()`, `toDateString()`, `toTimeString()`, and
  `toLocaleString()`, `toLocaleDateString()`, `toLocaleTimeString()` (always `en-US`).
- Dates compare with `<` (`Date` implements `Comparable`); `console.log` prints one as its ISO
  string, as Node does.

```ts
const start = new Date(Date.UTC(2024, 0, 31, 12));
const end = new Date(start);
end.setUTCDate(end.getUTCDate() + 1);
console.log(start.toISOString(), end.getUTCMonth(), start < end); // 2024-01-31T12:00:00.000Z 1 true
```

## Async and concurrency

`sleep(ms)`, `yieldNow()`, `spawn(p)`, `Promise.all`, `Promise.race`, `Promise.allSettled`
(with `PromiseSettledResult<T, E>`), `Promise.any`, `Promise.withResolvers` (with
`PromiseWithResolvers<T, E>`), `Promise.resolve`, `Promise.reject`, `shared(x)`, `Mutex<T>`,
`performance.now()` and `Date.now()` ([Async](../reference/async.md)), and the timer
functions `setTimeout`, `clearTimeout`, `setInterval` and `clearInterval` with their `Timer`
handle ([velt:timers](timers.md)).

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
