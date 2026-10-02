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
- There are no rest parameters and no overloads.
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

## Parameters

Parameters behave like `let` locals: reassigning one never affects the caller. Objects are
shared with the callee (`xs.push(1)` or `p.x = 2` inside the function is visible to the
caller); numbers, bools and strings are copies. Which parameters a function
modifies is inferred ([Memory model](memory.md#mutation-is-inferred)).

## Arrow functions and function types

- **Arrow functions** `(x: T) => expr` and `(x) => { … }` are closures. Parameter types are
  inferred where a function type is expected. There are no `function` expressions and no
  `this` rebinding: `this` inside an arrow is the enclosing method's `this`.
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
  cannot be a value.

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
  only calls it.
- A closure stored in a variable, field or array, or returned, is **escaping** and captures by
  value: objects are shared with it (the closure and the enclosing code see the same object),
  numbers and strings are copied. A variable that the closure or the enclosing code assigns
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
