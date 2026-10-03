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
type IteratorResult<T> = { done: false; value: T } | { done: true };

interface Iterator<T, E = never> {
  next(): IteratorResult<T> throws E;
  return(): void {}                       // early exit; the default does nothing
}

interface Iterable<T, E = never> {
  [Symbol.iterator](): Iterator<T, E>;
}
```

- `for (const x of src)` calls `src[Symbol.iterator]()` once, then `next()` until a result is
  `done`, binding each `value` (owned by the loop variable). `src` may be any type with that
  method, a class declaring `implements Iterable<T>` or not, or an `Iterable<T>` value.
- A finished result has no `value` (Velt has no `undefined`); `if (r.done)` narrows `r` like
  any [discriminated union](types.md#discriminated-unions).
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
    return { done: false, value: this.n + 1 };
  }

  return(): void {
    console.log(`stopped at ${this.n}`);
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
```

## `for await`

```ts
interface AsyncIterator<T, E = never> {
  next(): Promise<IteratorResult<T>, E>;
  async return(): Promise<void> {}       // early exit; the default does nothing
}

interface AsyncIterable<T, E = never> {
  [Symbol.asyncIterator](): AsyncIterator<T, E>;
}
```

- `for await (const x of src)` is allowed in async functions and
  [async generators](functions.md#async-generators); elsewhere it is an error that names the
  fix. It calls `src[Symbol.asyncIterator]()` once, then awaits `next()` until a result is
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
    return { done: false, value: this.left };
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
  or any expression of the discriminant's type (compared with `==`). Duplicates are an error.
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
