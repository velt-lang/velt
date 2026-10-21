# Functions and closures

## Declarations

```ts
function scale(xs: f64[], k: f64 = 2.0): f64[] {
  return xs.map((x) => x * k);
}
```

- `function name(p: T, q: U = default): R { … }`, optionally with a `throws E` clause after the
  return type ([Errors](errors.md)). Parameter types are required, except that a parameter with
  a default the type plainly shows has that type, as in TypeScript (`status = 302` is a
  `number`, `live = true` a `boolean`; the same literals, `new C(…)` and arrays of them that type
  [class fields](classes.md) without annotations). A missing return type is inferred from the
  body ([Return types](#return-types)).
- Default values work on functions, methods, constructors and interface methods; calls through
  an interface use the interface's defaults. A default is evaluated where the function is
  called, so it cannot use an earlier parameter yet (`q: number = p + 1` is an error saying so;
  take `q?: number` and write `const r = q ?? p + 1;` in the body).
- The default of a parameter of a dynamic type (`x: any = "d"`, `x: unknown = 0`) also applies to
  a passed `undefined`, as in JavaScript. The function applies it itself when the call starts,
  so it is the function's own default whichever way it is called: through an interface whose
  method takes `x?: any`, through a base class whose method the class overrides, through a
  function value (`const g = d; g(undefined)`), or as a callback (`xs.map((v: any = 0) => …)`).
  So does the default of a parameter whose type allows `undefined` (`x: number | undefined =
  5`, or `status = 302` where an interface declares `status?: number`), which is a `number` in
  the body. Any other default also applies to a passed `undefined`, tested where it is passed:
  `f(undefined)`, and `f(v)` with a `v` that may be `undefined`
  ([Null and undefined](types.md#null-and-undefined)).
- An optional parameter `q?: T` is `q: T | undefined = undefined`, as in TypeScript. Left out,
  it is `undefined`: it prints `undefined`, `typeof q` is `"undefined"`, `q === undefined` is
  true and `q === null` false. A call may also pass `undefined` for it (or a value that may be
  `undefined`). `q?: T | null` is `T | null | undefined`: left out it is `undefined`, and a call
  may pass `null`, which stays `null`. Passing `null` for a `q?: T` is an error (TS2322), as in
  TypeScript: leave the argument out or pass `undefined`.

  ```ts
  function show(z?: string, n?: number | null): string {
    return `${z} ${typeof z} ${n} ${n === null}`;
  }
  console.log(show());           // undefined undefined undefined false
  console.log(show("a", null));  // a string null true
  ```

  `q === null` on such a parameter is always `false`: tsc accepts it, and Velt warns, since an
  omitted argument is `undefined`. Write `q == null` to test for either nothing.
- A parameter of type `void` appears in function types and arrows (`(value: void) => void`),
  not in a `function` declaration (`function f(x: void)` is an error). Trailing `void`
  parameters may be left out, as in TypeScript: the `resolve` of a `Promise<void>` is called as
  `resolve()`, and `put(x: T)` on a `Box<void>` as `put()`. As in TypeScript, a call's own
  type arguments don't make a parameter optional: `h<void>()` for `function h<T>(x: T)` is an
  arity error (TS2554).
- A **rest parameter** `...xs: T[]` (the last one) collects the remaining arguments into an
  array, and a call may spread arrays into it: `sum(1, ...more, 4)` passes `[1, ...more, 4]`.
  Its type may be any array type: `readonly T[]`, or an alias of an array type
  (`...xs: Nums` with `type Nums = number[]`); another type is an error, as in TypeScript
  (TS2370). A rest parameter typed by a type parameter (`...args: A` with
  `A extends unknown[]`, which TypeScript infers as the tuple of the argument types) is not
  supported yet (#943).
  A module-level constant arrow function or function expression may have one too, typed the
  same ways: `const total = (...xs: Nums) => …` is the function `function total(...xs: Nums)`
  (its parameters must be typed). Rest parameters of other arrows and function expressions are
  not supported yet (see below).

  ```ts
  type Nums = number[];
  const total = (...xs: Nums): number => xs.reduce((a, b) => a + b, 0);
  const join = function (sep: string, ...parts: readonly string[]) {
    return parts.join(sep);
  };
  function main() {
    console.log(total(1, 2, 3), join("-", "a", "b")); // 6 a-b
  }
  ```

  The standard library's variadic functions (`Math.max`, `Math.min`, `Math.hypot`,
  `String.fromCharCode`, an array's `push`) accept a spread anywhere.
- A **spread into fixed parameters** passes the elements of a value whose length is known when
  compiling, as in TypeScript: a variable or field of a tuple type (`f(...t)` with
  `t: [number, string]` is `f(t[0], t[1])`) or an array literal (`f(...[1, 2])`). Too few or
  too many elements are the usual arity error, and a missing optional parameter takes its
  default. A spread of an array type (`T[]`) into fixed parameters is an error, as in
  TypeScript (JS would bind `undefined` to the parameters its elements do not fill). A tuple
  returned by a call or a getter (`f(0, ...g.pair)`, `f(...pair())`) is read once into a
  hidden temporary before the call, as JS reads it once, after the function or receiver
  (`getf()(...pair())` and `mk().m(...pair())` call `getf` and `mk` first; a receiver or a
  leading argument read through a getter, `h.g.m(...pair())` or `f(h.g, ...pair())`, is read
  first too, and so is a function a getter returns, `h.fnget(...pair())`). After another argument with effects (`f(next(), ...pair())`), which JS evaluates
  first, store it in a variable first (`const t = pair(); f(next(), ...t);`).

  ```ts
  function label(name: string, n: number, suffix?: string): string {
    return `${name}=${n}${suffix ?? ""}`;
  }
  const p: [string, number] = ["x", 4];
  console.log(label(...p), label(...p, "!")); // x=4 x=4!
  ```
- A function or method may have [overloads](#overloads) in TypeScript's form.
- **Nested functions** may be declared inside blocks, and are hoisted to the start of their
  block as in JS (a call before the declaration works). One that uses the enclosing function's
  locals (or another such nested function) is a closure, with the
  [captures](#captures) of an arrow function: it sees every later change of what it uses, may
  assign it, and may be passed on or returned as a value. Such closures may call themselves
  and each other (`mount` / `patch` / `unmount` of a renderer), async ones too. Nested
  functions calling each other this way share one environment holding what they use, so
  returning or storing them makes no reference cycle: the environment is freed with the last of
  them. Each one's parameter types and result type must be known before the bodies are
  checked, so one that returns a value needs its result type written (``…calls itself: not
  supported yet`` with ``write its result type``); without `return value;` it is `void`
  (`Promise<void>` when `async`). Not supported yet: a generator calling itself this way (pass
  the locals it uses as parameters, or declare it at module level), and a use before a local
  it uses is declared, which JS allows when the call runs later (declare the local first). A
  generic nested function cannot use the enclosing function's locals.

  ```ts
  function tally(words: string[]): string {
    const seen = new Map<string, number>();
    let total = 0;
    for (const w of words) add(w);   // before the declaration
    return `${total} ${seen.get("a")}`;

    function add(w: string): void {
      seen.set(w, (seen.get(w) ?? 0) + 1);
      total += 1;
    }
  }
  console.log(tally(["a", "b", "a"])); // 3 2

  function parity(base: number): Array<(n: number) => number> {
    function even(n: number): number {
      return n === 0 ? base : odd(n - 1);   // calls each other
    }
    function odd(n: number): number {
      return n === 0 ? -base : even(n - 1);
    }
    return [even, odd];
  }
  const [even, odd] = parity(7);
  console.log(even(4), odd(4)); // 7 -7
  ```

## Overloads

As in TypeScript, signatures without a body, written right before the implementation, give a
function or method several forms:

```ts
function describe(x: string): string;
function describe(x: number): number;
function describe(x: string | number): string | number {
  return typeof x === "string" ? `s:${x}` : x * 2;
}

function main() {
  const s: string = describe("a"); // the first signature: a `string`
  const n: number = describe(21); // the second: a `number`
  console.log(s, n + 1); // s:a 43
}
```

- A call takes the first signature, in order, that accepts its arguments (after inferring its
  type arguments, which must satisfy their bounds) and has that signature's result type. When
  none does, the error lists why each one didn't (TypeScript's TS2769). The implementation's
  own signature is not one of the forms: `describe(v)` with `v: string | number` is an error,
  as in TypeScript.
- Signatures may be generic and have optional and rest parameters. The implementation takes
  every signature's arguments: a signature it can't take is an error at the signature (TS2394).
  A signature with fewer parameters, or an optional one left out of a call, leaves the
  implementation's parameters out, which then take their defaults. A signature's parameters
  have no defaults of their own (TS2371). A type parameter of the implementation that a
  signature leaves open is `void` there (TypeScript's `unknown`).
- A signature may promise a narrower result than the implementation's: `T` out of `T | null`,
  one member of a union, a literal type out of its base (`"U"` out of `string`), a class out
  of a base class or an interface, or any of these in a `Promise` of an `async` implementation.
  The result is then tested, and another value panics
  (``overload `f` returned a `…` where its signature promises a `…` ``), where TypeScript would
  trust the implementation. Where the types already agree nothing is tested.
- Every signature is followed by the next one or by the implementation (TS2391), not by a
  function of another name (TS2389); all of them are `export`ed or none (TS2383), and a
  method's are all `static` or none (TS2387, TS2388). Signatures may be `async`, as the
  implementation is. Methods overload the same way, `static` ones too, and a subclass's
  override of the implementation is what the signatures call; `override` on the signatures
  overrides the base class's.
- A signature ends at its `;` or, as TypeScript's automatic semicolon insertion has it, at a
  line break (`function f(x: number): number` on a line of its own), as does a
  `declare function`.
- `import { f }`, a re-export and an export list (`export { f }`) bring the signatures along.
  Where a function type is expected (`const h: (s: string) => string = describe`, a callback
  argument), an overloaded function is the first signature that fits it; elsewhere
  (`const g = f`) it is its implementation.
- `velt doc` shows the signatures, not the implementation.
- Not supported yet: overloaded constructors, overloads of a function declared inside a
  function, overloaded methods in interfaces, and several declarations of one name that each
  have a body (overloads in Velt's own form, chosen by the most specific parameter types).

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

A `return;` next to `return value;` is not supported yet (TypeScript returns `undefined` there):
write `return undefined;` and give the function a `T | undefined` type.

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
([Generics](classes.md#generics)) or, as in TypeScript, other types (see below). A `const` type
parameter (`function tag<const K>(k: K)`, TypeScript 5.0) is accepted and treated as an ordinary
type parameter.

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

A bound that is not an interface is a type the type argument must be assignable to, as in
TypeScript: `T extends Record<string, V>`, `T extends readonly X[]`, `T extends string | number`,
`T extends "a" | "b"`, an alias of one (`type Primitive = string | number | boolean | null`), a
function type. The type argument is related to it as `tsc` relates types (structurally for object
types); one that is not assignable is an error (``the type `boolean` does not satisfy the
constraint `string | number` ``, TypeScript's TS2345 and TS2344). A literal argument for a type
parameter whose bound has literal types keeps its literal type (`same("a")` of
`same<T extends "a" | "b">` is `same<"a">`), also where the parameter's type contains the type
parameter (`t?: T`, `t: T | null`, `t: T[]`, `o: { k: T }`). An overloaded call takes the first
signature whose bounds its arguments satisfy ([Overloads](#overloads)), so the bounds choose the
form as they do in TypeScript:

```ts
type Primitive = string | number | boolean | null;

function wrap<T extends Primitive>(v: T): { value: T };
function wrap<T extends object>(v: T): { inner: T; tag: string };
function wrap<T>(v: T): { value: T } | { inner: T; tag: string } {
  return String(v) === "[object Object]" ? boxed(v) : plain(v);
}

function plain<T>(v: T): { value: T } {
  return { value: v };
}

function boxed<T>(v: T): { inner: T; tag: string } {
  return { inner: v, tag: "object" };
}

function same<T extends "a" | "b">(t: T): T {
  return t;
}

function main() {
  const n = wrap(5); // the first signature: `{ value: number }`
  const o = wrap({ st: "idle" }); // `{ st: string }` is no `Primitive`: the second
  const a: "a" = same("a");
  console.log(n.value, o.inner.st, o.tag, a); // 5 idle object a
}
```

Not yet: in the body, a value of such a type parameter has none of its bound's members
(`Object.keys(o)` on a `T extends Record<string, V>`, `xs[0]` on a `T extends X[]`; take the
bound's type as the parameter's where the body needs them, #1212), and a parameter has at most
one such bound (`T extends A & B` with both no interfaces).

A type parameter may have a **default** (`<T = string>`), as in TypeScript: a type parameter
nothing infers takes it, an inferred one ignores it, and explicit type arguments may leave out
the parameters that have defaults (`f<number>()` of `function f<A, B = A[]>()` is
`f<number, number[]>()`; the left-out ones are not inferred then). A default may use the type
parameters declared before it (and a method's, its class's), and a parameter without a default
may not follow one with a default:

```ts
function list<T = string>(): T[] {
  return [];
}

function pair<A, B = A[]>(a: A, b: B): [A, B] {
  return [a, b];
}

function main() {
  const names = list(); // string[]
  names.push("ada");
  const ns = list<number>(); // number[]
  ns.push(1);
  const p = pair<number>(1, [2, 3]); // pair<number, number[]>
  console.log(names, ns, p); // [ 'ada' ] [ 1 ] [ 1, [ 2, 3 ] ]
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

One growing call is allowed: in the `then` branch of `if (g(x))`, where `g` is a type predicate
whose type has only primitive members (`v is string | number | null`) and `x` has the type of
the parameter that grows, when the grown argument is an object type (a struct, class, object
type, array, tuple, map or function type). An instantiation whose argument is such an object type
does not compile that branch (`g(x)` still runs; if it returns `true`, which its type rules
out, the program stops with a `TypeError`), so the recursion ends there. An argument that may
hold a primitive (a union with a primitive member, `any`, `unknown`) keeps the branch.

```ts
type Primitive = string | number | boolean | null | undefined;
function isPrimitive(v: unknown): v is Primitive {
  return v === null || (typeof v !== "object" && typeof v !== "function");
}
function depth<T>(x: T): number {
  // `depth<{ value: T }>` has no `then` branch here: its argument is an object.
  if (isPrimitive(x)) return depth({ value: x }) + 1;
  return 0;
}

function main() {
  console.log(depth(3), depth([1])); // 1 0
}
```

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
  returns `{ done: true }`, and keeps doing so (`next().value` is then `null`; `undefined` in a
  `Generator<any>` or `Generator<unknown>`, as in JS). A `Generator<T, E>` is an `Iterator<T, E>` and
  an `Iterable<T, E>` (its `[Symbol.iterator]()` returns itself), so `for...of` takes it
  ([Iterables](control-flow.md#iterables)).
- The return type is required: `Generator<T>`, `Iterator<T>`, `Iterable<T>`,
  `IterableIterator<T>` or `IteratorObject<T>`, where `T` is the type of the yielded values; a
  call has that type, with the generator's error type as `E`.
  `return;` ends the generator; there is no `TReturn`, so `return value` is an error. A bare
  `yield` is allowed in a `Generator<void>` only.
- TypeScript's spellings `Generator<T, void>` and the same with `undefined`, `unknown` or `any`
  as `TReturn` mean `Generator<T>`, so TS signatures compile as they are; so do `Iterator`,
  `Iterable` and their async twins. Any other second type argument is the error type `E` and
  must be one (a class extending `Error`, a union of them, an interface or a type parameter):
  `Generator<number, string>` is an error naming the fix, since there it can only be TS's
  return type.
- **`next(value)`**: the third type argument of `Generator<T, X, N>` is TypeScript's `TNext`,
  the type of a `yield` expression's value and of `next(value)`'s argument
  (`Generator<number, void, string>`: `const s = yield 1` is a `string`). The first `next()`
  starts the body, so its value is ignored, as in JS. A `next()` without a value resumes a
  `yield` with `undefined` when `N` holds it (`unknown`, `any`, `T | undefined`); for another
  `N`, a `yield` whose value is used then panics (TS gives it `undefined` whatever `N` says).
  Without `N` (`Generator<T>`, also `void`, `undefined` or `never` as `N`), `next(value)` and
  using the value of a `yield` are errors, so a `yield` is a statement of its own (also as a
  branch of `c ? yield a : yield b`). The other protocol types (`Iterator<T, X, N>`, …) and
  async generators ignore `N`.
- **`throw(e)`** resumes the body with `e` thrown at the paused `yield`: a `try`/`catch` around
  it catches it (`e` is an `any` there, as `throw(e: any)` takes it) and the body goes on to
  its next `yield` or its end, which is what `throw()` returns; otherwise the generator is
  closed (its `finally` blocks run, as for `return()`) and `throw()` throws `e`. A generator
  that has not started, or has finished, throws `e` at once (and is done), as in JS. Only a
  program that calls `throw()` gets the code that handles it; there the `catch` of a `try`
  around a `yield` also catches an `any`.

  ```ts
  function* worker(): Generator<string, void, number> {
    try {
      const n = yield "ready";
      yield `got ${n}`;
    } catch (e) {
      yield `stopped: ${(e as Error).message}`;
    }
  }

  const w = worker();
  console.log(w.next().value, w.next(7).value);        // ready got 7
  const v = worker();
  v.next();
  console.log(v.throw(new Error("cancelled")).value);   // stopped: cancelled
  ```
- `yield* src` yields every value of `src`: another generator, an array, or any iterable
  `for...of` takes. In a generator that takes `next(value)` (or in a program that calls
  `throw()`), `yield*` of a generator of the same `T` (and `N`, or none) passes `next(value)`,
  `throw(e)` and closing on to it; an error it does not catch is thrown at the `yield*`.
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
- A generator's parameter defaults are evaluated when it is called, before the body runs, as
  in JS; a default of a parameter of a dynamic type (`x: any = "g"`) also applies to a passed
  `undefined`, whether the generator is called directly or through a function value
  (`const gv = gen; gv(undefined)`).
- A generator keeps its arguments (they are owned, as for an async function) and its locals
  between `yield`s. A generator method sees `this` as it is when the body runs, not when the
  method was called, as in JS. Calling `next()` or `return()` on a generator from inside its
  own body panics (`generator is already running`; JS throws a `TypeError`). A call keeps the
  generator alive while it runs: a body that drops the last reference to its own generator
  (`this.gen = other()`) finishes its step, and the generator is closed and freed after it.
- A generator cannot be copied or leave its thread: `clone()` of one, passing one to a spawned
  task (or sending it on a channel), putting one in `shared(...)` (also inside a `Mutex` or an
  object) and capturing one in an async closure that may run on another thread (see
  [Captures](#captures)) are errors, like for a promise. An async closure that stays on its
  task shares a generator it captured, as in JS (a stream's `pull` reading a generator:
  `async () => (await chunks.next()).value`). Behind an
  interface value (`Iterator<T>`) the compiler cannot see it, and handing it to another thread
  stops the program instead. Pass the arguments and create the generator where it is used.
- Not supported: a return value (TS's `TReturn`), `return(value)`, `await` in a (sync)
  generator (write an [async generator](#async-generators)), and `next(value)` / `throw()` on
  an async generator. `return()` returns `{ done: true }`, as in TS (where it may carry a
  value). Arrow functions cannot be generators (as in TS).
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
  Generator<T, E>` with the body's error type as `E` (a call never throws). Other `function`
  expressions are [function expressions](#function-expressions).
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

A parameter may be **destructured**, in functions, methods and arrows alike: `[a, b]: T` or
`{ a, b }: T`, with defaults (`{ x = 0 }`, `[a, b = 5]`) and holes (`[, c]`) inside the pattern
and a default for the whole parameter (`{ host, port }: Opts = defaults`). An arrow passed where
a function type is expected takes the pattern's type from it (`pairs.map(([k, v]) => …)`).
The body starts by destructuring the argument, exactly as `let [a, b] = arg;` would there, so a
destructured parameter costs what destructuring by hand does, and its bindings are `let`
variables (a closure may capture and assign them; each call has its own). As for `let`, an array
pattern may bind fewer elements than a tuple has, but not more (TS2493). A destructured
parameter can't be optional (`{ a }?: P`, TS2463) or have a type that includes `null`
(`{ a }: P | null`): `null` can't be destructured, and destructuring any value that may be
`null` is an error, in a `let` too (TS2339, or TS2488 for an array pattern). The defaults inside
a pattern run when the body starts, in order; since a later parameter's default is evaluated at
the call, before them, a default after a destructured parameter with defaults must be a constant
for now (`y = 10`, `s = "z"`, `xs = [1, 2]`).

```ts
function label([name, n]: [string, number], { sep = "=" }: { sep?: string }): string {
  return name + sep + n;
}

const counts = new Map<string, number>([["a", 1], ["b", 2]]);
console.log(label(["x", 4], {}), label(["y", 5], { sep: ":" })); // x=4 y:5
console.log([...counts.entries()].map(([k, v]) => k + v).join(",")); // a1,b2
```

Parameters behave like `let` locals: reassigning one never affects the caller. Objects are
shared with the callee (`xs.push(1)` or `p.x = 2` inside the function is visible to the
caller); numbers, bools and strings are copies. Which parameters a function
modifies is inferred ([Memory model](memory.md#mutation-is-inferred)).

## Function expressions

```ts
const twice = function <T>(x: T): T[] {
  return [x, x];
};
const later = async function (n: number): Promise<number> {
  return n + 1;
};

async function main() {
  const id = "xx-yx".replace(/[xy]/g, function (c) {
    return c === "x" ? "a" : "b";
  });
  console.log(id, twice(1), await later(1)); // aa-ba [ 1, 1 ] 2
}
```

- `function (params) { … }` as a value, optionally named (`function square(n) { … }`),
  `async` or generic, is the [arrow function](#arrow-functions-and-function-types) with the
  same parameters, result and body: parameter types may be left out where a function type is
  expected, as for an arrow.
- Its `this` is the object it is called on in TS. Without a `this` parameter, `this` in the body
  is TS's error ``'this' implicitly has type 'any' because it does not have a type
  annotation``, as under `--strict`, also in a generator (`function* () { … }`); with one
  (`function (this: T, …)`) it is not supported yet as a value (a function value has no
  receiver): declare the function and call it with `call` or `apply` (below), take the object as
  an ordinary parameter, or write a class method. In a function expression that is the value of
  an object-literal property (`{ v: 1, get: function () { return this.v; } }`), where TS types
  `this` as the literal, `this` is not supported yet either.
- A `this: T` parameter (first in a function's or method's parameter list) declares the type of
  `this` and is no parameter. `this: void` says the function doesn't use `this`. An arrow cannot
  have one (TS2730). `velt fmt` does not keep it yet, so it leaves a file with one as written
  and reports it. See [`this` parameters](#this-parameters) for function declarations.
- The name of a named expression inside its own body (recursion) is not supported yet: declare
  the function with `function name(…) { … }`.

## Arrow functions and function types

- **Arrow functions** `(x: T) => expr` and `(x) => { … }` are closures. Parameter types are
  inferred where a function type is expected. There is no `this` rebinding: `this` inside an
  arrow is the enclosing method's `this`.
- Arrow parameters take defaults and may be optional, like a function's: with
  `const fmt = (n: number, digits = 2) => n.toFixed(digits);`, `fmt(3.14159)` is `"3.14"`. A
  call through the `const` fills in left-out arguments; an unannotated parameter takes its
  default's type.
- As in TS, a function may take **fewer parameters** than the function type it is passed as:
  `xs.map((x) => x * 2)` where `map` passes `(x, i)`, and `xs.map(double)` with a one-parameter
  `double`, also a method value or a generic callback type (`xs.reduce(plus, 0)` and
  `xs.reduce(acc.add, 0)` with a two-parameter `plus` and `add`, where `reduce` passes
  `(acc, x, i)`). A function may also take more, when the extra ones are optional or have defaults
  (`setTimeout(tick, 10)` with `function tick(n?: number)`): they are left out.
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
- A function returning `T` is also accepted where the function type returns a union with `T`
  as a member (`(x: number) => Resp` for `(x: number) => Resp | Promise<Resp>`), as in TS; its
  result converts to the union. An error type parameter that nothing fixes
  (`<E>(h: (x: number) => Resp | Promise<Resp, E>)` called with a sync function) is `never`.
- A value whose type is a **union of function types** with the same parameters can be called:
  the call runs whichever function the value holds, and its result is the union of their
  results (a `void` member makes it `T | null`; `await` on that gives `void`). Members may take
  fewer parameters when theirs begin the longest member's: `f(1)` on
  `((a: number) => string) | (() => string)` passes `1` to the first and nothing to the second
  (the arguments are evaluated either way, in order). An arrow passed
  as such a union is typed by the member it fits, the first in order where several do.

  ```ts
  type Format = ((n: number) => string) | ((n: number) => number);

  function show(f: Format, n: number): string {
    const v = f(n);
    return typeof v === "string" ? `text ${v}` : `number ${v}`;
  }

  function main() {
    console.log(show((n) => `#${n}`, 3), show((n) => n * 2, 3)); // text #3 number 6
  }
  ```
- **Generic arrow functions** are written as in `.ts` files, `<T>(x: T): T => x` (the `.tsx`
  spelling `<T,>` works too, and JSX is allowed alongside). One must be the value of a `const`
  with typed parameters; it is then a generic function. Without a return type it returns the
  type of its body or its `return`s, by the rules of [Return types](#return-types)
  (`const id = <T>(x: T) => x` returns `T`; an `async` one returns `Promise<T>`). At module
  level it is an ordinary generic function; in a function body it is a generic function nested
  there, so each call instantiates it, and like any generic [nested function](#declarations) it
  cannot use the local variables around it (nor `this` in a method). A function value has one type, so using one as a value needs
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

- **Methods as values**: `obj.method` read without calling it, and `obj.method.bind(obj)`, are
  function values bound to `obj`: `xs.forEach(log.write)`, `const add = counter.add.bind(counter)`.
  The object is evaluated once, where the method is read, so a later assignment to the
  variable does not change which object it calls, as with `bind` in JS. The method dispatches
  like a call (an override in the object's class runs), and converts its arguments as a named
  function does (`[1, 2].forEach(p.log)` with `log(x: number)`). Read from `this` or a
  `const` or `using` local, it is the arrow `(x) => obj.method(x)` itself: it captures the
  object as that arrow would, and costs no allocation where such an arrow costs none. Two
  deliberate differences from JavaScript:
  - An unbound read: there `this` is lost (calling the value with a method that uses `this`
    throws a `TypeError`); Velt binds it to the object it was read from, so the two differ
    only where JavaScript would throw.
  - Identity: each read is a new function value, as each `bind` is in JavaScript, where an
    unbound read returns the one function of the prototype (`o.get === o.get` is `true`).
    So `===` and `!==` on a method read are errors: store it in a `const` first and use that,
    e.g. to register and remove a handler (`const tick = w.tick; e.on(tick); e.off(tick)`).

  `bind` takes exactly the object the method is read from
  (`a.m.bind(b)` is an error: write `(x) => b.m(x)`). A generic method, or one with a rest
  parameter, has no single function type: wrap it in an arrow.

  ```ts
  class Log {
    lines: string[] = [];
    write(s: string): void {
      this.lines.push(s);
    }
  }

  function main() {
    const log = new Log();
    const words: string[] = ["a", "b"];
    words.forEach(log.write.bind(log));
    const write = log.write;
    write("c");
    console.log(log.lines.join(",")); // a,b,c
  }
  ```

- **Function types** `(x: T) => U` accept closures and named functions alike. One that may
  throw says so: `(x: T) => U throws E` ([Errors](errors.md#dynamic-calls)). Calling a named
  function through a value behaves like calling it directly: an object it keeps or modifies is
  the caller's object. A promise passed through a function value is shared with the function,
  as any object is.
- **Planned** (#943): rest parameters in function types (`(event: string, ...args: T[]) => void`,
  and rest parameters of arrows and object-literal methods, `(...xs: T[]) => …`, other than
  a module-level constant's), generic
  function types (`<T>(description: string) => Token<T>`, a value each call instantiates)
  and construct signatures (`new (buffer: ArrayBuffer) => View`, a class passed as a value).
  They parse, and using one is an error saying it is not supported yet. A function type's
  parameter may be named by a destructuring pattern (`([a, b]: [number, number]) => number`),
  which, like a name, is only for the reader.
- A function type's parameter may be **optional** (`(app: App, options?: Options) => void`): its
  type is `Options | undefined`, as for a function's optional parameter, and a required parameter
  cannot follow it (TypeScript's TS1016). A call through such a value passes every argument for
  now; leaving the optional one out (`install(app)`) is not supported yet.

### `this` parameters

A function declaration whose body uses `this` may declare its type with a `this` parameter,
`function f(this: T, a: A)`, as in TypeScript. The caller passes `this`:
`f.call(obj, a)` and `f.apply(obj, [a])` run the body with `this` = `obj`, which must be a `T`
(TypeScript's TS2345 otherwise). A plain call `f(a)` passes `undefined`, which only `this: any`
and `this: unknown` accept; for another `T` it is TypeScript's TS2684 ("The 'this' context of
type 'void' is not assignable to method's 'this' of type 'T'"), also for `T | undefined`.

```ts
interface Counter {
  n: number;
}
function bump(this: Counter, by: number): number {
  this.n += by;
  return this.n;
}
const c: Counter = { n: 1 };
console.log(bump.call(c, 2), bump.apply(c, [3])); // 3 6
function kind(this: unknown): string {
  return typeof this;
}
console.log(kind(), kind.call("s")); // undefined string
```

A function with a `this` parameter that its body doesn't use is called like any other function.
Not supported yet: using a function whose body uses its `this` parameter as a value (`const g =
f`, `{ m: f }`, `xs.map(f)`), a function expression whose body uses one, one with overloads, a
generator with one, and `this` in a function type (`(this: T, a: A) => R` is `(a: A) => R` for
now).

### Functions with properties

A function can carry properties, as in JavaScript: an interface or object type with one call
signature and members (`interface Runner { (): number; stop(): void }`, or
`(() => number) & { stop(): void }`) types such a value. It is called like a function, its
members are read and written like an object's fields, `typeof` is `"function"`, and it is
accepted where its plain function type is expected. An interface extending a function type
(`interface Subscriber extends EffectFn { deps: string[] }`) is one too. It is built in one of
two ways:

- `Object.assign(fn, { … })`: typed by the expected type, or without one as TypeScript types
  it (`F & { … }`).
- A function cast to the type, followed by the writes of its members:
  `const r = (() => …) as Runner; r.stop = …;`. The writes right after the declaration give the
  members their values (declarations of other functions may come between them, since they run
  nothing); a required member with no write there is an error (Node would read `undefined`
  before the write).

```ts
interface Runner {
  (): number;
  stop(): void;
}

function makeRunner(): Runner {
  let count = 0;
  let stopped = false;
  const runner = (() => {
    if (!stopped) count += 1;
    return count;
  }) as Runner;
  runner.stop = () => {
    stopped = true;
  };
  return runner;
}

const r = makeRunner();
r();
r.stop();
console.log(r(), typeof r); // 1 function

const job = Object.assign((n: number) => n * 2, { id: 7 });
console.log(job(21), job.id, Object.keys(job)); // 42 7 [ 'id' ]
```

As in JavaScript, `JSON.stringify` leaves a function with properties out of an object and
writes `null` for it in an array; `JSON.stringify(f)` itself (Node's `undefined`) is not
supported yet. `Object.assign(fn, { … })` onto a variable makes a new object, where JavaScript
adds the properties to `fn` itself: using `fn` again after the call (other than in a closure) is
an error; use the call's result.

The value is an object with a fixed set of fields, the function among them: properties the type
does not declare can't be added, and a function whose type has no members has no properties of
its own (`const f = () => 1; f.label = "one"` is an error). Several call signatures next to
members (overloads) and `Object.defineProperty` onto a function are not supported yet.

### `call` and `apply`

`f.call(thisArg, a, b)` and `f.apply(thisArg, [a, b])` call a function value as JavaScript's
`Function.prototype` does: `f(a, b)`, with `thisArg` evaluated first (after `f`). `apply`'s
argument spreads into the call as `f(...args)` does: an array literal or a tuple fills the
parameters one by one, and an array of unknown length only a rest parameter
(`String.fromCharCode.apply(null, codes)`, `Math.max.apply(null, xs)`), as TypeScript's
`strictBindCallApply` checks it.

`this` matters only to a function that reads it. A function declaration with a [`this`
parameter](#this-parameters) runs on the `thisArg`. Other function declarations, arrow functions and the
static functions of built-in namespaces read none, so any `thisArg` works for them
(`Math.max.apply(Math, xs)`), except `Promise`'s, which take `Promise`. A method read as a value
(`o.m`, `C.m`) keeps the object or class it was read from: `o.m.call(o, x)` works, and `null` or
`undefined` works when the method doesn't use `this`. A variable holding a method read as a value
(`const f = o.m`) takes no other `this`; other function values, such as parameters, take `null` or
`undefined`, as `f(x)` calls them. Any other `this` is not supported yet (an error): Velt would
run the function on the object it keeps, where Node uses the `this` given or throws.

```ts
function add(a: number, b: number): number {
  return a + b;
}

const codes = [104, 105];
const pair: [number, number] = [3, 4];
console.log(String.fromCharCode.apply(null, codes)); // hi
console.log(add.call(null, 1, 2), add.apply(null, pair)); // 3 7
```

## Captures

- A closure passed directly as a call argument, or called immediately, is **non-escaping** and
  captures by reference: `xs.forEach((x) => { total += x; })` updates `total`, as in JS.
- Parameters of function type are borrowed unless the callee keeps them. One that the body
  stores, returns, captures in an escaping closure (`spawn(async () => cb())`), copies or
  passes to an async function is **owned** (inferred like other parameters), and a closure
  literal passed to an owned parameter is escaping, so `new Holder(() => msg)` keeps a closure
  that stays valid. Closures and overridden or interface methods have fixed borrowed
  parameters; they may still keep a function they receive, as JS callback registries do
  (`onCleanup((fn) => (cleanupFn = fn))`, `subscribe(handler) { this.handlers.push(handler); }`).
  A program that does so passes such callees only closures that own their environment: every
  closure literal passed to a function value or an interface method escapes (also one that
  forwards a borrowed function, below), and a parameter of a named function that is kept, or
  passed to a function value, an interface method, an overridden method or a parameter of a
  generic type, becomes owned (a generic type may stand for a function type, so a closure
  literal passed to a parameter of a generic type always escapes, and a method keeping a value
  of a generic type, `put(t: T) { this.items.push(t); }`, counts as keeping a function), also when a closure
  inside the function does it (`untrack(() => topic.subscribe(f))`). A closure may likewise keep
  a function it captured from a local variable (`untrack(() => registry.add(onInvalidate))`).
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
  and that parameter of the enclosing function becomes owned. In a closure, or an overridden or
  interface method, where that parameter cannot become owned, it is kept as described above.
  What remains an error (``cannot keep a copy of `g`, a borrowed function parameter``) is keeping
  a function that reached the closure through a destructured or `for...of` binding of a
  borrowed value, or through `this`.
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
  a plain copy. A closure created inside another closure that assigns a variable the outer
  closure captured (`let last = ""; const f = (v: string) => { const g = () => { last = v; };
  g(); }`) assigns the variable itself, as in JS: it lives in a cell that the function declaring
  it and every closure on the way share. A variable holding a promise cannot live in a cell, so
  a closure created inside another closure may not assign one the outer closure captured
  (``…is assigned by a closure created inside another closure, and cannot be shared with it``);
  assign it in the function that declares it. A `for (let …)` loop's step runs on a fresh
  binding per iteration, as in JS, so the same error applies to a variable the loop's head
  declares; a variable declared in the loop body is one per iteration and may be assigned. So
  is a variable a `for (let … of …)` or `for await` loop declares (over an array, a `Map`, a
  `Set` or a generator), as in JS: each iteration gets its own cell, so the closures the
  iterations create do not share one (`for (let x of ["a", "b"]) fs.push(() => { x += "!";
  return x; })` gives `a!` and `b!`). The same holds for the variables a destructuring `let`
  declares, each time it runs. A closure called on the spot or a callback (`[1, 2].forEach(()
  => { x += "!"; })`) that assigns such a variable changes that variable too, never the array
  element it was bound from: the array keeps its contents, as in JS. Such a variable cannot live
  in a cell when it holds a promise or a spawned closure also captures it; a closure assigning
  it then gets ``cannot assign to `x`, which borrows an array element``: copy it into a `let`
  declared in the loop body and assign that. A closure passed to `push` is stored, so it is
  escaping too.
- An async closure that stays on the task that created it captures like any other escaping
  closure, as in JavaScript: it may change what it captured, the enclosing code may assign the
  variables it captured, and every call sees the same objects and variables. Each call shares
  the captured objects with the closure (a count increment, no copy), and a variable that the
  closure assigns, or that the enclosing code assigns after creating it, lives in a cell. Every
  call runs as a started promise on the caller's task, so it interleaves with the rest of the
  task only at `await`s, never in parallel ([Async](async.md#promises)).
- An async closure that may run on another thread copies what it captured for each call, and
  may not modify a captured variable or object, itself or through a closure it creates (``this
  async closure modifies captured `n`, so it must stay on the task that created it``, with where
  it leaves its task); nor may the enclosing code assign one it captured (``cannot assign to `k`
  after a stored closure captured it``), nor a closure created in another closure (``…is
  assigned by a closure created inside another closure, and cannot be shared with it``). The
  compiler proves which closures stay: one may leave when it is spawned
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

### A closure using the constant it initializes

A closure created in a `const`'s initializer may use that constant, as in JavaScript: an
object literal whose methods return the object, a callback that stops its own timer, a
function that refers to itself.

```ts
interface Counter {
  n: number;
  inc(): Counter;
}

function makeCounter(): Counter {
  const c: Counter = {
    n: 0,
    inc() {
      c.n += 1;
      return c;               // the object being initialized
    },
  };
  return c;
}

function show(): void {
  const fact = (n: number): number => (n <= 1 ? 1 : n * fact(n - 1));
  console.log(makeCounter().inc().inc().n, fact(5));   // 2 120
}
show();
```

- An arrow constant that names itself (`fact` above), with typed parameters (or a function
  type annotation giving them, `const fib: (n: number) => number = (n) => …`), is the function
  `function fact(n: number): number { … }`: at module level a module function, in a body a
  nested function (which may use the enclosing function's variables, see
  [Declarations](#declarations)). Nothing refers to itself, so nothing leaks. An arrow using
  `this` stays a closure (a function would have a `this` of its own).
- Any other closure (an object literal method, an arrow or function expression) that uses the
  constant it is part of captures the variable, which the constant's value fills in once the
  initializer has run, and reads it when it runs. Without an annotation, the constant's type
  is the initializer's type with the closure checked first; when that type depends on the
  constant itself (a method returning it without a written result type), write the type, as
  TypeScript requires there too (TS7022, TS7023):
  ``type annotation needed for `c`: a closure in its initializer uses it``.
- A closure reading the constant while its initializer is still running (it is called from
  the initializer) stops the program with JavaScript's error, `ReferenceError: Cannot access
  'c' before initialization`. A read of the value checks this where it happens, so a read
  on a path that does not run checks nothing (`ready ? x : 1`); a use of the object in place (a
  field read, a method call) checks at the start of its statement (in an expression-bodied
  arrow, when the arrow is called). A closure created inside such a closure shares the
  constant too. In Velt this is a
  panic, which `catch` does not intercept.
- An object holding a closure that captures the object is a reference cycle, which reference
  counting does not free: each such object stays allocated until the program exits (one per
  constant initialized, for instance one per component created). Break the cycle when the
  object is done with: give it a `[Symbol.dispose]()` that clears the closure fields (or
  replaces them with closures that capture nothing) and declare it with `using`, or call that
  method yourself. A recursive arrow constant (above) holds no cycle.
- Not supported yet: a closure that runs on another task (a timer callback, a spawned
  closure) using the constant it initializes (`const t = setInterval(() => clearInterval(t),
  5)` reports ``…is used by a closure in its own initializer, which cannot share it``), and a
  module-level constant other than a recursive arrow whose closures use it
  (``module-level constant refers to itself``) (#1292). The cycle is tracked in #1291.
