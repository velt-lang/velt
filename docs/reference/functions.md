# Functions and closures

## Declarations

```ts
function scale(xs: f64[], k: f64 = 2.0): f64[] {
  return xs.map((x) => x * k);
}
```

- `function name(p: T, q: U = default): R { … }`, optionally with a `throws E` clause after the
  return type ([Errors](errors.md)). Parameter types are required; a missing return type
  means `void`.
- Default values work on functions, methods, constructors and interface methods; calls through
  an interface use the interface's defaults.
- An optional parameter `q?: T` is `q: T | null = null`.
- A **rest parameter** `...xs: T[]` (the last one) collects the remaining arguments into an
  array, and a call may spread arrays into it: `sum(1, ...more, 4)` passes `[1, ...more, 4]`. A
  spread argument must land in the rest parameter (in JS `f(...xs)` would bind `xs[0]` to the
  first parameter); the standard library's variadic functions (`Math.max`, `Math.min`,
  `Math.hypot`) accept a spread anywhere.
- There are no overloads.
- **Nested functions** may be declared inside blocks but cannot capture locals
  (``` `x` cannot be captured by a nested function```); use an arrow function.

## Generic functions

`function f<T, U extends Bound>(…)` is monomorphized: every instantiation is compiled
separately, so there is no boxing and bounds resolve to direct calls. Type arguments are
inferred or given explicitly (`f<f64>(2)`). Bounds are interfaces
([Generics](classes.md#generics)).

Type arguments are inferred from the arguments first and, as in TypeScript, from the expected
type of the call (an annotated variable, a return statement, a typed parameter) second. The
expected type types the arguments of type parameters it fixes, before an untyped number
literal falls back to `i64`; where an argument's own type disagrees, the argument decides:

```ts
import { Set } from "velt:collections/set";

function id<T>(x: T): T {
  return x;
}

function main() {
  const y: i32 = id(1); // T = i32
  const z: f64 = id(2); // T = f64
  const s: Set<u8> = new Set([1, 2]); // T = u8
  const c: Map<string, u16> = new Map([["a", 1]]);
  console.log(y, z, s.size, c.get("a"));
}
```

Because each instantiation is compiled, a generic function may call itself (directly or
through other generic functions) with the same type arguments, but not with growing ones:
`f<T>` calling `f<T[]>` would need `f<T[][]>`, `f<T[][][]>` and so on without end. The
compiler reports this at the growing call, like TypeScript's "type instantiation is
excessively deep":

```ts error
function nest<T>(x: T, n: i64): i64 {
  // error: instantiating `nest<T[]>` from `nest<T>` grows without end
  return n == 0 ? 0 : nest<T[]>([x], n - 1);
}
```

Recurse with a fixed type instead: a non-generic helper, or a `JsonValue` for data whose
shape is only known at run time.

## Generators

```ts
function* range(from: i64, to: i64): Generator<i64> {
  for (let i = from; i < to; i++) {
    yield i;
  }
}

function* evens(limit: i64): Generator<i64> {
  for (const i of range(0, limit)) {
    if (i % 2 == 0) {
      yield i;
    }
  }
}

for (const i of evens(7)) {
  console.log(i);                          // 0 2 4 6
}
const g = range(1, 3);                     // nothing has run yet
console.log(g.next(), g.next().value, g.next().done);  // { value: 1, done: false } 2 true
```

- `function* name(…): Generator<T>` is a **generator**, and so is a method written `*name()`,
  `static *name()` or `*[Symbol.iterator]()`. Calling one creates a `Generator<T, E>`
  ([prelude](../std/prelude.md#iteration)) without running the body. Each `next()` runs the
  body up to its next `yield v` and returns `{ value: v, done: false }`; when the body ends it
  returns `{ done: true }`, and keeps doing so (`next().value` is then `null`). A `Generator<T, E>` is an `Iterator<T, E>` and
  an `Iterable<T, E>` (its `[Symbol.iterator]()` returns itself), so `for...of` takes it
  ([Iterables](control-flow.md#iterables)).
- The return type is required: `Generator<T>`, `Iterator<T>`, `Iterable<T>`,
  `IterableIterator<T>` or `IteratorObject<T>`, where `T` is the type of the yielded values; a
  call has that type, with the generator's error type as `E`.
  `return;` ends the generator; there is no `TReturn`, so `return value` is an error. A bare
  `yield` is allowed in a `Generator<void>` only, and a `yield` has no value (`const x = yield
  1` is an error: there is no `next(value)`), so it is a statement of its own (also as a
  branch of `c ? yield a : yield b`).
- TypeScript's spellings `Generator<T, void>`, `Generator<T, void, unknown>` (any `TNext`) and
  the same with `undefined`, `unknown` or `any` as `TReturn` mean `Generator<T>`, so TS
  signatures compile as they are; so do `Iterator`, `Iterable` and their async twins. Any other
  second type argument is the error type `E` and must be one (a class extending `Error`, a union
  of them, an interface or a type parameter): `Generator<number, string>` is an error naming the
  fix, since there it can only be TS's return type.
- `yield* src` yields every value of `src`: another generator, an array, or any iterable
  `for...of` takes.
- **Errors**: `E` is what the body throws, inferred like a function's `throws` (or written:
  `Generator<T, E>`, also through a type alias such as `type Gen = Generator<i64, A>`, or
  `throws E` after the return type). `next()` throws it, and so does
  `for...of` over the generator; creating the generator never throws. A generator that threw is
  done. A generator method implementing an interface whose result names an error type writes
  it: `class Lines implements Iterable<string, IoError>` declares `*[Symbol.iterator]():
  Iterator<string, IoError>`. Each class keeps its own `E`: calling a generator method never
  throws, so implementations of one interface method don't share an error type.
- **Closing**: `return()` (which `for...of` calls when it is left early), the end of a `using`
  block (`using g = gen();`), and dropping the generator all close a generator suspended at a
  `yield`: its `finally` blocks run and its `using` values are disposed, as if the `yield` were
  a `return`. A generator that never started or already finished has nothing to clean up.
  Because closing runs `finally` blocks where the generator can neither pause, report an
  error, nor go on with its body, a `finally` block in a generator cannot `yield`, throw, or
  `break` / `continue` to a loop outside it (TS allows all three).
- A generator keeps its arguments (they are owned, as for an async function) and its locals
  between `yield`s. A generator method sees `this` as it is when the body runs, not when the
  method was called, as in JS. Calling `next()` or `return()` on a generator from inside its
  own body panics (`generator is already running`; JS throws a `TypeError`). A call keeps the
  generator alive while it runs: a body that drops the last reference to its own generator
  (`this.gen = other()`) finishes its step, and the generator is closed and freed after it.
- A generator cannot be copied or leave its thread: `clone()` of one, passing one to a spawned
  task (or sending it on a channel), putting one in `shared(...)` (also inside a `Mutex` or an
  object) and capturing one in an async closure are errors, like for a promise. Behind an
  interface value (`Iterator<T>`) the compiler cannot see it, and handing it to another thread
  stops the program instead. Pass the arguments and create the generator where it is used.
- Not supported: `next(value)` (TS's `TNext`), `throw()`, and `await` in a (sync) generator
  (write an [async generator](#async-generators)). `return()` returns `{ done: true }`, as in
  TS (where it may carry a value). Arrow functions cannot be generators (as in
  TS).
- **Cost**: `for (const x of gen(a))` with a direct call (or over a class whose
  `[Symbol.iterator]` is a generator method) keeps the generator's state in the loop: no
  allocation, no `IteratorResult` objects, and the body is resumed by a direct call that the
  optimizer can inline; such a loop runs as fast as the equivalent hand-written loop. A
  generator used as a value is one heap object; each `next()` then returns a small
  `IteratorResult` value. Spreading a direct call (`[...gen(a)]`) and destructuring one
  (`const [x, y] = gen(a)`) use the same loop: only the resulting array is allocated.

### Generator function expressions

```ts
const evens = function* (limit: i64): Generator<i64> {
  for (let i = 0; i < limit; i += 2) {
    yield i;
  }
};

function main() {
  const step = 10;
  const tens = function* (n: i64): Generator<i64> {
    for (let i = 0; i < n; i++) {
      yield i * step;
    }
  };
  console.log([...evens(5)], [...tens(3)]);  // [ 0, 2, 4 ] [ 0, 10, 20 ]
}
```

- `function* (…): Generator<T> { … }` and `async function* (…): AsyncGenerator<T> { … }` are
  generator function values, optionally named (`function* walk(…)`); their type is `(…) =>
  Generator<T, E>` with the body's error type as `E` (a call never throws). Velt has no other
  `function` expressions: write an [arrow function](#arrow-functions-and-function-types), which
  TS code can do too.
- At module level, `const g = function* (…) { … };` is the generator function `g`.
- Elsewhere it is a closure that uses the variables around it as JS does: its generators see
  the objects it captured (`xs.push(3)` after creating a generator shows up in it), a variable
  assigned after the expression (by the function or by a generator) is one variable that all
  of them see, and the body may assign it (`count++`). The name of a named expression is not in scope in its body (TS allows recursion through it):
  declare a `function*` to recurse. Type parameters and rest parameters are errors there too;
  declare a `function*`.
- The return type is required, as for any generator.

## Async generators

```ts
async function* pages(n: i64): AsyncGenerator<string> {
  for (let i = 1; i <= n; i++) {
    await sleep(1);                        // e.g. fetch the page
    yield `page ${i}`;
  }
}

async function main() {
  for await (const p of pages(3)) {
    console.log(p);                        // page 1, page 2, page 3
  }
}
```

- `async function* name(…): AsyncGenerator<T>` is an **async generator**, and so is a method
  written `async *name()`, `static async *name()` or `async *[Symbol.asyncIterator]()`. Its
  body may both `await` and `yield`. Calling one creates an `AsyncGenerator<T, E>`
  ([prelude](../std/prelude.md#iteration)) without running the body; each `next()` returns a
  `Promise<IteratorResult<T>, E>` that runs the body to its next `yield`, awaiting what it
  awaits on the way. It is an `AsyncIterator<T, E>` and an `AsyncIterable<T, E>`, so
  [`for await`](control-flow.md#for-await) takes it.
- The return type is required: `AsyncGenerator<T>`, `AsyncIterator<T>`, `AsyncIterable<T>`
  or `AsyncIterableIterator<T>` (a sync result type on an `async function*`, or an async one on a `function*`, is an error
  naming the fix).
- `yield* src` delegates to an async iterable (another async generator) and, as in JS, to a
  sync one (a generator, an array).
- `yield p` where `p` is a promise (`Promise<T, E2>`) awaits it before yielding its value, as in
  JS: `yield fetchPage(i)` yields the page. If it rejects, the error is thrown at the `yield`,
  and `E2` joins the generator's error type.
- **Errors**: `E` is what the body throws, awaited calls included, inferred or written
  (`AsyncGenerator<T, E>`). `next()` rejects with it, and `for await` rethrows it, whether it
  is thrown before or after the body's first `await`. A generator that threw is done.
- **Closing**: `return()` (which `for await` awaits when it is left early) and `await using g =
  agen()` close a generator suspended at a `yield` like a sync one, and here its `finally`
  blocks may `await` (so may `await using` disposals in the body); `return()` resolves once
  they are done. A `using` block's end and dropping the generator close it without awaiting:
  if that cleanup would `await`, it is cancelled instead (its values are dropped, its `finally`
  blocks do not run), like a [cancelled async function](async.md#cancellation). A `finally`
  block cannot `yield`, throw or `break` out, as in a sync generator.
- **Overlapping calls** are queued, as in JS: a `next()` or `return()` made while an earlier
  call is still running waits for its turn, and each promise settles with its own step, in
  call order. A call nobody awaits (a `Promise.race` loser, a call abandoned by `timeout`) still
  takes its turn; its value is dropped and the next call gets the next one.
- A stored generator is closed when its last reference goes, like any object
  ([Memory](memory.md)): when the variable's last use is a `next()` call, that is when the
  call's promise settles, since the call holds the generator until then. JS never closes an
  abandoned generator (its `finally` blocks never run); Velt has no garbage collector to wait
  for, so it closes it as soon as nothing can resume it:

  ```ts
  async function* ticks(): AsyncGenerator<i64> {
    try {
      yield 1;
      yield 2;
    } finally {
      console.log("ticks closed");
    }
  }

  async function main() {
    const g = ticks();
    const p = g.next();                    // the last use of `g`: `p` holds it now
    // `p` settled at once (the body reached `yield 1` without awaiting), which released the
    // last reference: "ticks closed" has been printed.
    console.log("waiting");
    const r = await p;
    if (!r.done) {
      console.log(r.value);                // 1
    }
  }
  ```

  To keep it open, use it again later (`await g.return()` closes it explicitly).
- An async generator belongs to the task that created it, like a started promise: it cannot be
  copied or passed to another task (see above), also not as an `AsyncIterable<T>` or
  `Iterable<T>` value made at the call (`spawn(sum(gen()))`). An interface value made earlier
  and stored is not looked into: passing one backed by a generator to `spawn` panics
  (`a generator cannot be copied`).
- **Cost**: `for await (const x of agen(a))` with a direct call (or over a class whose
  `[Symbol.asyncIterator]` is an async generator method) keeps the generator's state inside the
  enclosing async function's state, like `await f()` does: no allocation for the generator or
  per item, and each step is a direct call of the generator's poll function. A stored
  generator is one heap object; `await g.next()` on an `AsyncGenerator<T>` variable is a direct
  call too, with the result as a small `IteratorResult` value. Through an `AsyncIterator<T>` or
  `AsyncIterable<T>` interface value, each `next()` allocates its promise.

## Parameters

```ts
function sum(...xs: number[]): number {
  return xs.reduce((a, b) => a + b, 0);
}

const more = [2, 3];
console.log(sum(), sum(1, ...more, 4), Math.max(...more)); // 0 10 3
```

Parameters behave like `let` locals: reassigning one never affects the caller. Objects are
shared with the callee (`xs.push(1)` or `p.x = 2` inside the function is visible to the
caller); numbers, bools and strings are copies. Which parameters a function
modifies is inferred ([Memory model](memory.md#mutation-is-inferred)).

## Arrow functions and function types

- **Arrow functions** `(x: T) => expr` and `(x) => { … }` are closures. Parameter types are
  inferred where a function type is expected. There are no `function` expressions and no
  `this` rebinding: `this` inside an arrow is the enclosing method's `this`.
- Arrow parameters take defaults and may be optional, like a function's: with
  `const fmt = (n: number, digits = 2) => n.toFixed(digits);`, `fmt(3.14159)` is `"3.14"`. A
  call through the `const` fills in left-out arguments; an unannotated parameter takes its
  default's type.
- As in TS, a function may take **fewer parameters** than the function type it is passed as:
  `xs.map((x) => x * 2)` where `map` passes `(x, i)`, and `xs.map(double)` with a one-parameter
  `double`. An arrow may also take more, when the extra ones have defaults.
- **Generic arrow functions** are written as in `.ts` files, `<T>(x: T): T => x` (the `.tsx`
  spelling `<T,>` works too, and JSX is allowed alongside). One must be a module-level `const`
  with typed parameters and a return type; it is then a generic function:

  ```ts
  const firstOr = <T>(xs: T[], fallback: T): T => (xs.length > 0 ? xs[0].clone() : fallback);

  function main() {
    console.log(firstOr([3, 4], 0), firstOr([], "none"));
  }
  ```

- **Function types** `(x: T) => U` accept closures and named functions alike. One that may
  throw says so: `(x: T) => U throws E` ([Errors](errors.md#dynamic-calls)). Calling a named
  function through a value behaves like calling it directly: an object it keeps or modifies is
  the caller's object. A function whose parameter takes ownership of a promise (one owner)
  cannot be a value, and neither can a generic function that keeps a parameter whose type is a
  promise at the value's type arguments or still depends on a type parameter.

## Captures

- A closure passed directly as a call argument, or called immediately, is **non-escaping** and
  captures by reference: `xs.forEach((x) => { total += x; })` updates `total`, as in JS.
- Parameters of function type are borrowed unless the callee keeps them. One that the body
  stores, returns, captures in an escaping closure (`spawn(async () => cb())`), copies or
  passes to an async function is **owned** (inferred like other parameters), and a closure
  literal passed to an owned parameter is escaping, so `new Holder(() => msg)` keeps a closure
  that stays valid. Closures and overridden or interface methods have fixed borrowed
  parameters, so they cannot keep a function they receive:
  ``cannot keep a copy of `g`, a borrowed function parameter``.
  The executor of `new Promise((resolve, reject) => …)` is the exception: its `resolve` and
  `reject` are handles the compiler creates on the heap (never a caller's closure), so it may
  keep them ([Async](async.md#new-promise)). A spawned or stored closure can't capture a borrowed
  function parameter either (same error).
- A closure literal passed directly to a function value or an interface method (whose
  parameters are fixed, so it may keep them, for example as a generic `T`) is escaping: it
  owns a heap environment. Unless it captures a borrowed function parameter of the enclosing
  function, as a middleware forwarding its `next` does (`inner.handle(req, (r) => next(r))`):
  then it stays non-escaping, like the parameter it forwards, and is safe as long as the callee
  only calls it. A parameter declared with a generic type (`keep(x: T)`, with `T` a function
  type) may be kept, and so may anything passed to a callee that returns a promise the caller
  does not `await` right away (the callee may run after the caller returned): a forwarding
  closure passed there escapes like any other. It captures the function it forwards by value,
  and that parameter of the enclosing function becomes owned. Where that parameter cannot become
  owned (in a closure, or an overridden or interface method), this is the error
  ``cannot keep a copy of `next`, a borrowed function parameter``.
- A closure stored in a variable, field or array, or returned, is **escaping** and captures by
  value: objects are shared with it (the closure and the enclosing code see the same object),
  numbers and strings are copied. A captured object the enclosing code does not use again moves
  into the closure, so it is released (and disposed) when the closure is, even if other captures
  are still used afterwards. A variable that the closure or the enclosing code assigns
  while the other still uses it (`let count = 0; const inc = () => { count++; }; inc();
  console.log(count)`) lives in a shared, reference-counted cell, so both see every change, as
  in JS; a closure that is the only remaining user (a `makeCounter` returning `() => ++n`) keeps
  a plain copy. A `for (let …)` loop's step runs on a fresh binding per iteration, as in JS. A
  closure passed to `push` is stored, so it is escaping too.
- Async closures never modify captured variables, because they may run on another thread
  ([Async](async.md#thread-safety)), and the enclosing code may not assign a variable an async
  closure captured (``cannot assign to `k` after a stored closure captured it``): the closure
  keeps its own copy.

```ts
function apply(f: (x: i64) => i64, v: i64): i64 {
  return f(v);
}

function makeCounter(): () => i64 {
  let n = 0;
  return () => {
    n += 1;                     // the closure owns `n`
    return n;
  };
}

function scale(xs: f64[], k: f64 = 2.0): f64[] {
  return xs.map((x) => x * k);
}

let total = 0;
[1, 2, 3].forEach((x) => {
  total += x;                   // non-escaping: captures `total` by reference
});
const next = makeCounter();
next();
console.log(total, next(), apply((x) => x * 10, 5), scale([1.5]));   // 6 2 50 [ 3 ]
```
