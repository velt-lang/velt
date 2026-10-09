# Functions and closures

## Declarations

```ts
function scale(xs: f64[], k: f64 = 2.0): f64[] {
  return xs.map((x) => x * k);
}
```

- `function name(p: T, q: U = default): R { … }`, optionally with a `throws E` clause after the
  return type ([Errors](errors.md)). Parameter types are required; a missing return type is
  inferred from the body ([Return types](#return-types)).
- Default values work on functions, methods, constructors and interface methods; calls through
  an interface use the interface's defaults.
- An optional parameter `q?: T` is `q: T | null = null`.
- A **rest parameter** `...xs: T[]` (the last one) collects the remaining arguments into an
  array, and a call may spread arrays into it: `sum(1, ...more, 4)` passes `[1, ...more, 4]`.
  The standard library's variadic functions (`Math.max`, `Math.min`, `Math.hypot`) accept a
  spread anywhere.
- A **spread into fixed parameters** passes the elements of a value whose length is known when
  compiling, as in TypeScript: a variable or field of a tuple type (`f(...t)` with
  `t: [number, string]` is `f(t[0], t[1])`) or an array literal (`f(...[1, 2])`). Too few or
  too many elements are the usual arity error, and a missing optional parameter takes its
  default. A spread of an array type (`T[]`) into fixed parameters is an error, as in
  TypeScript (JS would bind `undefined` to the parameters its elements do not fill); a tuple
  returned by a call or a getter is stored in a variable first (`const t = pair(); f(...t);`),
  since JS reads it once.

  ```ts
  function label(name: string, n: number, suffix?: string): string {
    return `${name}=${n}${suffix ?? ""}`;
  }
  const p: [string, number] = ["x", 4];
  console.log(label(...p), label(...p, "!")); // x=4 x=4!
  ```
- There are no overloads.
- **Nested functions** may be declared inside blocks but cannot capture locals
  (``` `x` cannot be captured by a nested function```); use an arrow function.

## Return types

As in TypeScript, a function or method without a return type returns the type of its `return`
expressions (exported ones too):

- one type when they agree, or the one the others convert to: `return 1` and `return 0.5` give
  `f64`, a class and its base class give the base;
- otherwise their union (`return "positive"` and `return n` give `string | i64`), made
  nullable by a `return null`;
- `void` when no `return` has a value, `never` when every returned value never completes
  (`return fail()`);
- `Promise<T>` for an `async` function, `T` from its returns.

Integers inferred this way are JavaScript numbers, as in TypeScript: `(await half())/2` and
a narrowed `T | null` result divide like `number`s, and a method returning integers that a
subclass overrides returns `number` (`f64`), so an override may return `2.5`.

```ts
function describe(n: i64) {
  if (n > 0) {
    return "positive";
  }
  return n; // describe returns string | i64
}

async function double(n: i64) {
  return n * 2; // Promise<i64>
}

function main() {
  console.log(describe(3), describe(-1)); // positive -1
}
```

An unannotated method that overrides a base class method or implements an interface method
returns that method's type, and its `return`s are checked against it. Arrow functions follow
the same rules when no function type is expected; where one is, its result type applies.
Generic arrow functions (`const id = <T>(x: T) => x`) follow them too.

A `return;` next to `return value;` is an error: TypeScript would return `undefined`, which
Velt doesn't have. Return `null` and give the function a `T | null` type instead.

A function may use itself (or other functions whose return types are being inferred) anywhere
outside its `return` expressions, as in TypeScript: `count` below returns `i64`. Only when its
`return` expressions depend on the function itself, directly, through a local (`const m = f(x);
return m;`) or through other functions whose `return` expressions use it in turn (`isEven`
returning `isOdd(n - 1)`, which returns `isEven(n - 1)`), it needs an annotation, as
TypeScript's "implicitly has return type 'any'" does:

```ts
class TreeNode {
  kids: TreeNode[] = [];
}

function count(n: TreeNode) {
  let total = 1;
  for (const c of n.kids) {
    total += count(c); // fine: not in a `return` expression
  }
  return total;
}

function main() {
  console.log(count(new TreeNode())); // 1
}
```

```ts error
function fib(n: i64) {
  // error: function `fib` needs a return type annotation
  if (n < 2) {
    return n;
  }
  return fib(n - 1) + fib(n - 2);
}
```

Write the type: `function fib(n: i64): i64`. A function without a `return` value is `void`
before its body is checked, so it may call itself freely (a recursive `walk(child);`). A body
that uses itself outside its `return`s is checked twice: once to find the return type, then
against it.

## Generic functions

`function f<T, U extends Bound>(…)` is monomorphized: every instantiation is compiled
separately, so there is no boxing and bounds resolve to direct calls. Type arguments are
inferred or given explicitly (`f<f64>(2)`). Bounds are interfaces
([Generics](classes.md#generics)).

Type arguments are inferred from the arguments first and, as in TypeScript, from the expected
type of the call (an annotated variable, a return statement, a typed parameter) second. The
expected type types the arguments of type parameters it fixes, before an untyped number
literal falls back to `i64`; where an argument's own type disagrees, the argument decides,
unless the result would then not convert to the expected type: a type parameter the arguments
fixed to a type that converts to the expected one takes the expected one, and the arguments
convert to it (`const ns: Named[] = wrap(new C())` calls `wrap<Named>`;
`const ps: (i64 | null)[] = pair(1, 2)` calls `pair<i64 | null>`):

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

An arrow function argument is checked against its parameter type with the type parameters the
expected type fixed, so its parameters need no annotations there either:

```ts
function id<T>(x: T): T {
  return x;
}

function main() {
  const inc: (x: i32) => i32 = id((x) => x + 1); // x: i32
  const lengths: ((s: string) => number)[] = id([(s) => s.length]);
  console.log(inc(1), lengths[0]("abc")); // 2 3
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
- The return type is required, as for any generator, unless the expression is written where
  a function type is expected (`return function* () { … }` in a function returning `() =>
  Generator<string>`, or `const g: Gen = function* () { … }`): then it is that type's result,
  as for an arrow.

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
  inferred where a function type is expected. There are no `function` expressions other than
  [generator function expressions](#generator-function-expressions), and no
  `this` rebinding: `this` inside an arrow is the enclosing method's `this`.
- Arrow parameters take defaults and may be optional, like a function's: with
  `const fmt = (n: number, digits = 2) => n.toFixed(digits);`, `fmt(3.14159)` is `"3.14"`. A
  call through the `const` fills in left-out arguments; an unannotated parameter takes its
  default's type.
- As in TS, a function may take **fewer parameters** than the function type it is passed as:
  `xs.map((x) => x * 2)` where `map` passes `(x, i)`, and `xs.map(double)` with a one-parameter
  `double`. An arrow may also take more, when the extra ones have defaults.
- As in TS, a function that returns a value is accepted where a **`void`-returning** function
  type is expected; the value is evaluated and dropped. This holds for an arrow without a
  return type (its expression body, or a `return value;` in it) and for a named function or a
  function value passed or assigned there. An arrow or function annotated `: void` still may
  not return a value, as in TS.

  ```ts
  function each(xs: string[], f: (s: string) => void) {
    for (const x of xs) {
      f(x);
    }
  }

  function main() {
    const seen: string[] = [];
    each(["a", "b"], (s) => seen.push(s));
    each(["c\n"], (s) => process.stdout.write(s)); // c
    console.log(seen); // [ 'a', 'b' ]
  }
  ```
- **Generic arrow functions** are written as in `.ts` files, `<T>(x: T): T => x` (the `.tsx`
  spelling `<T,>` works too, and JSX is allowed alongside). One must be the value of a `const`
  with typed parameters; it is then a generic function. Without a return type it returns the
  type of its body or its `return`s, by the rules of [Return types](#return-types)
  (`const id = <T>(x: T) => x` returns `T`; an `async` one returns `Promise<T>`). At module
  level it is an ordinary generic function; in a function body it is a generic function nested
  there, so each call instantiates it, and like any [nested function](#declarations) it cannot
  use the local variables around it (nor `this` in a method). A function value has one type, so using one as a value needs
  a function type to instantiate it at (`const f: (x: i64) => i64 = id;`), and a generic arrow
  anywhere else (an argument, a `let`) is an error:

  ```ts
  const firstOr = <T>(xs: T[], fallback: T): T => (xs.length > 0 ? xs[0].clone() : fallback);

  function main() {
    console.log(firstOr([3, 4], 0), firstOr([], "none"));
    const pair = <A, B>(a: A, b: B) => `${a}:${b}`; // returns string
    console.log(pair(1, true), pair("x", 2.5)); // 1:true x:2.5
    const id = <T>(x: T) => x;
    const n: i64 = id(41) + 1;
    console.log(n); // 42
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
- A closure held in a `const` that is only ever called (`const add = (n: number) => {
  this.total += n; }; add(1); add(2);`) is non-escaping too: it captures by reference, sees
  every later change of what it captures, and lives in the frame, so capturing `this` does not
  make the class reference-counted. The compiler proves it: the closure's variable is used only
  as `f(...)` (never copied, stored, returned, passed on or captured by another closure), the
  enclosing function and the closure are not `async` or generators, it captures no `using`
  value and nothing holding a promise, no closure created inside it captures a variable the
  enclosing function assigns, and no call runs while a reference into a
  captured object is held (the call's own arguments do not use what it captures, and it is not
  inside a `for...of` over such an object, a `match` on one, or next to an argument borrowing
  one). Otherwise it is escaping, as below; the results are the same, only the cost differs.
  Borrowing never makes a program an error: where a closure borrowing `this` (or `const me =
  this`) would conflict with a caller's borrow, such as a `for...of` over `c.items` around a
  `c.clear()` that replaces `items`, the closure and `me` share instead. The fallback is for
  the whole program: one such conflict makes every held closure and every `const me = this` in
  it share, as if none of them borrowed.
- A closure stored in a `let`, a field or an array, held in a `const` that is used other than
  by calling it (or one that does not meet the conditions above), or returned, is **escaping**
  and captures by
  value: objects are shared with it (the closure and the enclosing code see the same object),
  numbers and strings are copied. A captured object the enclosing code does not use again moves
  into the closure, so it is released (and disposed) when the closure is, even if other captures
  are still used afterwards. A variable that the closure or the enclosing code assigns
  while the other still uses it (`let count = 0; const incs = [() => { count++; }]; incs[0]();
  console.log(count)`) lives in a shared, reference-counted cell, so both see every change, as
  in JS; a closure that is the only remaining user (a `makeCounter` returning `() => ++n`) keeps
  a plain copy. A `for (let …)` loop's step runs on a fresh binding per iteration, as in JS. A
  closure passed to `push` is stored, so it is escaping too.
- An async closure that stays on the task that created it captures like any other escaping
  closure, as in JavaScript: it may change what it captured, the enclosing code may assign the
  variables it captured, and every call sees the same objects and variables. Each call shares
  the captured objects with the closure (a count increment, no copy), and a variable that the
  closure assigns, or that the enclosing code assigns after creating it, lives in a cell. Every
  call runs as a started promise on the caller's task, so it interleaves with the rest of the
  task only at `await`s, never in parallel ([Async](async.md#promises)).
- An async closure that may run on another thread copies what it captured for each call, and
  may not modify a captured variable or object (``this async closure modifies captured `n`, so
  it must stay on the task that created it``, with where it leaves its task); nor may the
  enclosing code assign one it captured (``cannot assign to `k` after a stored closure captured
  it``). The compiler proves which closures stay: one may leave when it is spawned
  (`spawn(async () => …)`, `spawn(f())`, an argument of a spawned call), is an HTTP handler, goes
  into `shared(...)` or a `Mutex`, is sent on a channel or settles a promise; when a parameter it
  is passed to (a generic one included), a variable holding it, a closure capturing it or a
  task returning it does; when it is stored in an
  object, array or map whose type reaches one of those places; and when it is passed directly to
  a function value or an interface or overridden method, which may keep it. A timer callback
  (`setTimeout`) runs as a spawned task, so it is one too.

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

```ts
class Ctx {
  count: number = 0;
}

// The closure outlives `methods`, changes `ctx` and keeps `calls` in a cell.
function methods(ctx: Ctx): (by: number) => Promise<number> {
  let calls: number = 0;
  return async (by: number): Promise<number> => {
    calls += 1;
    ctx.count += by;
    await sleep(1);
    return ctx.count * 100 + calls;
  };
}

async function main() {
  const ctx = new Ctx();
  const inc = methods(ctx);
  console.log(await Promise.all([inc(1), inc(2)]), ctx.count);   // [ 302, 302 ] 3
}
```
