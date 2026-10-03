# Control flow

## Statements

- `if` / `else if` / `else`, `while`, `do … while`, C-style `for` (comma lists allowed:
  `for (let i = 0, j = n; i < j; i++, j--)`), `for (const x of xs)`,
  [`for await (const x of xs)`](#for-await) in async code, `break` and `continue`
  (optionally labeled: `outer: for (…)` … `continue outer;`), `return`, blocks, and the
  ternary `?:`. A body without braces (`if (c) return x;`) is a one-statement block.
- Conditions take `bool` or nullable values ([safe truthiness](variables.md#conditions-safe-truthiness)).

## `for...of`

- `for...of` iterates arrays, maps (`[key, value]` pairs), strings (their characters, as in
  JS), classes with an `entries()` method, and **iterables**: values with a
  `[Symbol.iterator]()` method returning an `Iterator<T>` (see [below](#iterables)).
- The loop variable is each element itself (objects are references): you can call its methods
  (including modifying ones), assign its fields, and store it elsewhere, which shares the
  element. Index the array to replace an element.
- Iterating a temporary (a call result, `await …`, a literal) **consumes** it: each element is
  handed to the loop variable without a count.
- There is no `for...in`; iterate `map.keys()` or an object's known fields.

### Iterables

The prelude declares TypeScript's iteration protocol, without `undefined`:

```ts
type IteratorResult<T> = { value: T; done: false } | { done: true };

interface Iterator<T, E = never> {
  next(): IteratorResult<T> throws E;
  return(): IteratorResult<T> {           // early exit; the default does nothing else
    return { done: true };
  }
}

interface Iterable<T, E = never> {
  [Symbol.iterator](): Iterator<T, E>;
}
```

- `for (const x of src)` calls `src[Symbol.iterator]()` once, then `next()` until a result is
  `done`, binding each `value` (owned by the loop variable). `src` may be any type with that
  method, a class declaring `implements Iterable<T>` or not, or an `Iterable<T>` value.
- A finished result has no `value` (Velt has no `undefined`); `if (r.done)` narrows `r` like
  any [discriminated union](types.md#discriminated-unions). Reading `r.value` without narrowing
  gives `T | null`: `null` when `r` is done (TS: `undefined`), so `g().next().value` works.
- `return()` returns an `IteratorResult<T>`, as in TS (`{ done: true }`; the loop ignores it).
  An iterator class that releases something there writes `return(): IteratorResult<T>` and
  returns `{ done: true }`, which also works in Node (where a `return()` returning nothing
  makes `break` throw a `TypeError`).
- **Typed errors**: `E` is what `next()` throws (`never`, the default, means nothing). The loop
  throws it, so a function iterating an `Iterable<T, IoError>` throws `IoError`; a generic
  `function sum<E>(xs: Iterable<i64, E>): i64 throws E` throws what its argument does.
- **Early exit**: leaving the loop before `done` (`break`, `return`, a thrown error, or a
  labeled `break` / `continue` of an outer loop) calls the iterator's `return()` exactly once,
  as in JS, so an iterator holding a resource can release it. Running to the end, `continue`,
  and an error thrown by `next()` itself do not call it.
- An iterator is not itself iterable: iterate the iterable that creates it (as in TS, where
  `for...of` needs `[Symbol.iterator]()`).
- A [generator](functions.md#generators) (`function*`) is the short way to write an iterable:
  `for (const x of gen(a))` over a direct call needs no iterator object at all, and a class
  whose `[Symbol.iterator]` is a generator method (`*[Symbol.iterator]()`) is iterable without
  an iterator class. Leaving the loop early closes the generator (its `finally` blocks run).
- `AsyncIterator<T, E>` (`next(): Promise<IteratorResult<T>, E>`) and `AsyncIterable<T, E>`
  (`[Symbol.asyncIterator]()`) are their async counterparts, iterated with
  [`for await`](#for-await).
- **Builtin iterables**: arrays, strings, `Map`s and `Set`s (`velt:collections/set`) are
  `Iterable<T>` (`Iterable<[K, V]>` for a map, `Iterable<string>` for a string), so they convert
  to an `Iterable<T>` value and satisfy an `Iterable<T>` bound: `sum(xs: Iterable<number>)`
  takes `[1, 2, 3]`, a set, `m.values()` or (with `Iterable<string>`) a string. An array
  literal written where an `Iterable<T>` is expected holds `T`s. `for...of` over them directly
  keeps its own loops; only code written against `Iterable<T>` goes through the protocol.
  Converting an array to an `Iterable<T>` value copies no elements (the value refers to the
  array; at most its 24-byte header is boxed), and each loop over it creates one iterator.
- `x[Symbol.iterator]()` on them returns an `Iterator<T>` (TS: `ArrayIterator<T>` and so on;
  the prelude classes `ArrayIterator<T>` and `StringIterator` are its implementations). An
  array's iterator is a live view, as in JS: it reads the length at each step, so it visits
  elements pushed meanwhile, and once it reported `done` it stays done. A string's yields
  characters (code points). A map's and a set's iterate the entries as of the call, like
  `for...of` over a map (JS's map and set iterators are live views).
- `IterableIterator<T, E>`, `IteratorObject<T, E>` and `AsyncIterableIterator<T, E>` are TS's
  iterators that are also iterable (`[Symbol.iterator]()` returns the iterator itself, declared
  as `Iterator<T, E>`, since Velt has no covariant returns). Generators implement them and may
  be declared to return them (`function* f(): IterableIterator<number>`), and such a value
  converts to an `Iterable<T, E>` / `AsyncIterable<T, E>`. It does not convert to an
  `Iterator<T, E>` yet (interface values don't convert to the interfaces they extend); call
  `it[Symbol.iterator]()` for one.
- TS's two-argument `IteratorResult<T, TReturn>` is `IteratorResult<T>` when `TReturn` means
  "nothing" (`void`, `undefined`, `unknown`, `any`); another `TReturn` is an error, since a
  finished result carries no value. The second argument of `IterableIterator`,
  `IteratorObject` and `AsyncIterableIterator` follows `Generator`'s rules: it is dropped when
  it means "nothing", and is the error type `E` otherwise.

```ts
class Countdown implements Iterator<i64> {
  n: i64;

  constructor(n: i64) {
    this.n = n;
  }

  next(): IteratorResult<i64> {
    if (this.n == 0) {
      return { done: true };
    }
    this.n -= 1;
    return { value: this.n + 1, done: false };
  }

  return(): IteratorResult<i64> {
    console.log(`stopped at ${this.n}`);
    return { done: true };
  }
}

class From implements Iterable<i64> {
  start: i64;

  constructor(start: i64) {
    this.start = start;
  }

  [Symbol.iterator](): Iterator<i64> {
    return new Countdown(this.start);
  }
}

for (const n of new From(3)) {
  console.log(n);                         // 3 2 1
}
for (const n of new From(5)) {
  if (n == 4) {
    break;                                // prints "stopped at 3"
  }
}

function total(xs: Iterable<number>): number {
  let sum = 0.0;
  for (const x of xs) {
    sum += x;
  }
  return sum;
}

const prices = new Map<string, number>([["tea", 2.5], ["cake", 4.0]]);
console.log(total([1, 2, 3]), total(prices.values()));   // 6 6.5

const it = ["a", "b"][Symbol.iterator]();
console.log(it.next());                   // { value: 'a', done: false }
```

### Consuming an iterable

Everything that takes the values of an array takes whatever `for...of` takes: a string's
characters, a `Map`'s entries (and those of a class with `entries()`), a `Set`'s elements, a
generator, any iterable. It uses the same loop as `for...of` (so a direct generator call needs
no generator object, and stopping early closes the iterator):

- **Spread**: `[...gen()]`, `[0, ...it, 9]`, and spread arguments of a rest parameter
  (`sum(...gen())`, `Math.max(...values())`). The values are taken in order, where the spread
  stands.
- **`Array.from(src)`** and **`Array.from(src, (value, i) => ...)`** over anything `for...of`
  takes; the callback runs as each value arrives.
- **Array destructuring**: `const [a, b] = gen()` takes only the values the pattern needs, then
  closes the iterator (`return()`, a generator's `finally`) if it has more, as JS does;
  `...rest` takes the remaining values, and a default applies when the iterable ended early
  (`[x = 0]`). Also in `for...of` heads: `for (const [a, b] of rows())` over iterables of
  iterables.
- **`new Map(iterable)`** of `[key, value]` pairs and **`new Set(iterable)`**
  ([velt:collections/set](../std/collections/set.md)).
- An object literal with a `*[Symbol.iterator]()` method is an iterable
  ([Iterable object literals](types.md#iterable-object-literals)).

```ts
function* countTo(n: i64): Generator<i64> {
  for (let i = 1; i <= n; i++) {
    yield i;
  }
}

function* squares(n: i64): Generator<[i64, i64]> {
  for (const v of countTo(n)) {
    yield [v, v * v];
  }
}

function sum(...xs: i64[]): i64 {
  let total = 0;
  for (const x of xs) {
    total += x;
  }
  return total;
}

console.log([0, ...countTo(3)]);                        // [ 0, 1, 2, 3 ]
console.log(sum(...countTo(4)));                         // 10
console.log(Array.from(countTo(3), (v, i) => v * 10 + i)); // [ 10, 21, 32 ]
const [first, second] = countTo(100);                    // takes two values, then closes it
console.log(first, second);                              // 1 2
const bySide = new Map(squares(3));
console.log(bySide.get(3));                              // 9
console.log([...bySide.keys()], [..."héllo"]);           // [ 1, 2, 3 ] [ 'h', 'é', 'l', 'l', 'o' ]
const [head, ...tail] = "abc";
console.log(head, tail, Math.max(...bySide.values()));  // a [ 'b', 'c' ] 9
```

Arrays keep their own (faster) spread and destructuring: a spread array is copied with one
allocation, and destructuring an array reads its elements by index. A string or a `Map` is
consumed as the array of its characters or entries, as `for...of` iterates it.

## `for await`

```ts
interface AsyncIterator<T, E = never> {
  next(): Promise<IteratorResult<T>, E>;
  async return(): Promise<IteratorResult<T>> {   // early exit; the default does nothing else
    return { done: true };
  }
}

interface AsyncIterable<T, E = never> {
  [Symbol.asyncIterator](): AsyncIterator<T, E>;
}
```

- `for await (const x of src)` is allowed in async functions,
  [async generators](functions.md#async-generators) and a script's top-level statements (whose
  generated `main` is then `async`, [Scripts](modules.md#scripts-top-level-statements));
  elsewhere it is an error that names the fix. It calls `src[Symbol.asyncIterator]()` once, then awaits `next()` until a result is
  `done`. `src` may be any type with that method (a class, an `AsyncIterable<T>` value, an
  [async generator](functions.md#async-generators)).
- **Typed errors**: the loop rethrows what `next()` rejects with (`E`), like `for...of`.
- **Early exit**: leaving the loop before `done` (`break`, `return`, a thrown error, a labeled
  `break` / `continue` of an outer loop) awaits the iterator's `return()` exactly once, as in
  JS; for an async generator that runs its `finally` blocks, which may `await` themselves.
- Over a **sync** source (an array, an iterable, a generator) `for await` works like JS too:
  each value that is a promise is awaited (`for await (const v of [load(a), load(b)])`), other
  values are used as they are. An array of promises is consumed: its promises move into the
  loop, so a variable holding the array cannot be used after it.
- `for await (const x of agen(a))` over a direct async generator call keeps the generator's
  state inside the enclosing async function's: no allocation (see
  [Cost](functions.md#async-generators)).

```ts
class Ticks implements AsyncIterable<i64> {
  n: i64;

  constructor(n: i64) {
    this.n = n;
  }

  [Symbol.asyncIterator](): AsyncIterator<i64> {
    return new TickIter(this.n);
  }
}

class TickIter implements AsyncIterator<i64> {
  left: i64;

  constructor(left: i64) {
    this.left = left;
  }

  async next(): Promise<IteratorResult<i64>> {
    await sleep(1);
    if (this.left == 0) {
      return { done: true };
    }
    this.left -= 1;
    return { value: this.left, done: false };
  }
}

async function main() {
  for await (const t of new Ticks(3)) {
    console.log(t);                       // 2 1 0
  }
}
```

## `switch`

`switch` has JavaScript semantics: the discriminant is evaluated once; the first `case` whose
value equals it is entered (`default` when none does, wherever it is written); bodies fall
through until `break`, `return`, `throw` or `continue`. `continue` cannot target a `switch`; a
labeled `switch` can be left with `break label` from nested loops. Each case body is its own
block scope.

- **Case values**: literals (numbers, also negative ones; strings; bools), `null`, enum members,
  or any expression that `===` can compare with the discriminant (and compared like it: a
  `string | null` case value on a `string` discriminant, or the reverse; a `string` constant on a
  union of string literals). A local of a literal type (`const y: "y" = "y"`) acts like the
  literal: it narrows and counts toward exhaustiveness. On a discriminant (`switch (s.kind)`) and
  on `typeof x`, other case values must still be literals. Duplicates are an error.
- **Narrowing**: `switch (x.kind)` on a discriminated union narrows `x` in each case (a case
  reached by fallthrough sees the union of the members that can get there); `switch (typeof x)`
  narrows like `typeof` tests; `switch (x)` on a union narrows by literal member and by
  `case null`.
- **Exhaustiveness**: without `default`, a `switch` on a discriminant, a `typeof`, a union of
  literals or an enum must cover every possible member. A missing one is an error listing the
  cases (``missing cases: "rect", "tri"``), and a complete one needs no code after it. In
  `default`, the value is narrowed to the members no case took; with none left it is `never`,
  so `const _x: never = s;` checks exhaustiveness the TypeScript way.
- **Performance**: cases on tags, enums and integers dispatch through one jump table; string
  cases compare in order.

```ts
enum Level { Debug, Info, Warn }

function describe(n: i64): string {
  let s = "";
  switch (n) {
    case 1:
      s += "one ";              // falls through
    case 2:
      s += "two";
      break;
    default:
      s = "other";
  }
  return s;
}

function tag(l: Level): string {
  switch (l) {
    case Level.Debug:
      return "D";
    case Level.Info:
      return "I";
    case Level.Warn:
      return "W";
  }
}

function label(x: string | null): string {
  switch (x) {
    case null:
      return "none";
    default:
      return x;                 // x is a string here
  }
}

outer: for (let i = 0; i < 3; i++) {
  for (let j = 0; j < 3; j++) {
    if (j == 1) {
      continue outer;
    }
    console.log(i, j, describe(i), tag(Level.Info), label(null));
  }
}
```
