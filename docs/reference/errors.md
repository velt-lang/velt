# Errors

`throw`, `try`, `catch` and `finally` have JavaScript syntax and TypeScript-style types, and they
compile to zero-cost result returns: every throwing call is "call, check a tag, branch". There
is no unwinding and there are no exception tables. Errors are **typed and checked**: the
compiler knows exactly what each function and each `try` block can throw.

## Throwing

- `throw expr` throws a value of any type, usually a class extending the prelude `Error`, which
  has a `message` field. Calling a throwing function outside `try` makes the caller throw it
  too.
- As in JavaScript, an error has a `name` (`"Error"` unless a subclass assigns its own:
  `this.name = "AppError"` in the constructor), and `String(e)`, `` `${e}` `` and `e.toString()`
  give `"<name>: <message>"` (only the name when the message is empty, only the message when the
  name is). `name` is `Error`'s: a subclass may declare it again (`name: string;`), give it an
  initializer (`name = "AppError"`, which runs after `super(...)` in its place among the class's
  other initializers; a subclass's runs after its base class's) or redefine it with a getter
  (`get name() { … }`), which `toString()` then reads. A union of error classes
  (`AppError | TypeError`, a `catch` variable) and a nullable error have `e.name` and
  `e.toString()` too, and are written the same way in templates, by `String(e)` and by string
  concatenation (`null` for `null`, `undefined` for `undefined`); so is an error held by an `any`
  or an `unknown` (`` `${e}` `` is `Error: boom`, or a subclass's own `toString()`). `e.stack` is
  a string that starts as Node's does, with `"<name>: <message>"`; Velt keeps no call frames, so
  the `    at …` lines Node adds after it are not there (assigning `stack` is not supported yet).
  Node fixes the text the first time `stack` is read; Velt writes it on every read, so after
  `e.name = "Renamed"` a later read gives `Renamed: …` in Velt and `Error: …` in Node.
  Arrays of errors are written as
  JavaScript writes them (`` `${errors}` ``, `errors.join(", ")`: each element's `toString()`).
  `console.log(e)` and `JSON.stringify(e)` leave the name out. The standard error classes
  `TypeError`, `RangeError`, `SyntaxError`, `ReferenceError`, `EvalError` and `URIError` extend
  `Error` with their own names; throw, catch, test (`instanceof`) and extend them like any
  error class:

  ```ts
  class AppError extends Error {
    code: string;
    constructor(message: string, code: string) {
      super(message);
      this.name = "AppError";
      this.code = code;
    }
  }

  function check(n: number): number {
    if (n < 0) {
      throw new RangeError(`${n} is negative`);
    }
    if (n > 100) {
      throw new AppError("too large", "E1");
    }
    return n;
  }

  try {
    check(500);
  } catch (e) {
    console.log(String(e), e instanceof RangeError); // AppError: too large false
  }
  ```
- A function's **error type** is the union of everything it can throw. It is inferred over the
  whole program (also through recursion), or written as a **`throws` clause** after the return
  type: `function load(id: string): User throws NotFound | Forbidden`.
- A written clause bounds the body
  (``` `load` throws `Timeout`, which its `throws` clause does not allow ```; a subclass is
  allowed by its base class) and is the function's error type even when the body throws less.
  `throws unknown` (or `any`, and `Promise<T, unknown>` for a promise) allows any thrown value,
  as JavaScript does: `null`, a string, any class; a `catch` gets it as an `unknown` to narrow.
- Field initializers may throw: `new C(…)` (and a struct literal that leaves the field out)
  evaluates them, so it throws what they throw, including inherited ones, besides what the
  constructor throws. They run as part of the constructors
  ([Classes](classes.md#classes)), so a constructor's `throws` clause must allow what the
  initializers it runs throw.
- Methods, constructors (`constructor(x: T) throws E`), arrows (`(x: T): R throws E => …`) and
  async functions (`async function f(): Promise<T> throws E`, see [Async](async.md#errors))
  take a `throws` clause; `declare function` cannot.

## Catching

- In `catch (e)`, `e` has the **exact union** of what the `try` block can throw
  (`NotFound | Forbidden`; `never` when nothing can throw). Narrow it with `instanceof`,
  `typeof`, `==` or `switch` like any [union](types.md#union-types).
- As in TypeScript, the binding may be annotated `unknown` or `any` (`catch (e: unknown)`); `e`
  still has the union of what the block can throw. Any other annotation is TypeScript's error
  TS1196.
- An `if`/`else if` chain whose branches leave handles every member, so no code is needed after
  it.
- `throw e;` rethrows; a narrowed `e` rethrows just that member.
- `catch { … }` without a binding and `finally` (which runs on every path) work as in JS.
- A `finally` runs after the `return`, `break`, `continue` or throw that leaves its `try`, so it
  sees what they took: an object, array or string passed on before is shared and still usable
  there. A value that can't be shared, such as a promise, is gone once a `return` hands it on,
  and using it in the `finally` is ``use of moved value``.
- `instanceof` also tells apart the subclasses of a member: a function declared
  `throws AppError` is caught as an `AppError`, and `if (e instanceof NotFound) … else if (e
  instanceof Timeout) …` narrows it to each subclass
  ([downcasts](classes.md#instanceof-downcasts)). Such a chain does not cover every member
  (an `AppError` may be neither), so code after it still needs a result.

## Dynamic calls

Function types carry an error type: `(x: T) => U throws E`. A written `throws` clause bounds
what a function stored there may throw.

- A function type written without `throws` (as TypeScript writes them) throws what the
  closures stored into it throw, inferred over the whole program: a closure that throws may be
  used as a value of such a type (a field, parameter, variable, interface member, array element
  or `Map` value), and calls through the type throw its errors, so a `try` around them catches
  them as in JavaScript. The error type belongs to the written type: every value of
  `revive: (s: string) => T` in `interface Reviver<T>` throws what any closure stored into one
  throws, whatever `T` is. Each written type is inferred on its own: a closure passed for a
  parameter, stored into a field or a typed variable, or returned from a function with a
  written function result throws into that type only, and a value read from one such type
  and stored into another (a parameter passed on, stored into a field) takes its errors
  along. Where the compiler can't tell which written type a value comes from (a callback
  passed through a function value, an element of a `Map`), every function type of the same
  shape gets the errors. When nothing stored into the type throws, it throws nothing
  and calls through it need no error check. For a type whose result is a `Promise`, the
  inferred errors are the promise's rejection type. As in JavaScript, any thrown
  value is inferred: a closure that throws `null`, a string or an `Error | null` gives the type
  those errors, and a `catch` around a call through it sees their union (`unknown` in
  TypeScript terms; narrow it with `instanceof`, `typeof` or `=== null`). An object type made
  for one check (`throw { code: 1 }`) still needs the error type written
  (`() => void throws { code: number }`). The errors also come through other such types (a closure calling through one) and through
  recursion, and a `let` without a type initialized with a closure (`let f = () => {}`) is
  such a type too. Function types of the standard library are not inferred this way: the
  timers (`setTimeout`, `setInterval`, `setImmediate`) and `queueMicrotask` take a callback
  that may throw (what it throws is uncaught, as in Node), and a proxy trap or a promise
  executor that throws is reported.

  ```ts
  interface Reviver<T> {
    name: string;
    revive: (s: string) => T;
  }

  const url: Reviver<URL> = { name: "URL", revive: (s) => new URL(s) };
  console.log(url.revive("https://example.com/a").pathname); // /a
  try {
    url.revive("not a url");
  } catch (e) {
    console.log((e as Error).name); // TypeError
  }
  ```
- A closure gets the error type of the function type expected where it is written (and must
  not throw more than a written `throws` allows); otherwise, what its body throws. A named
  function used as a value may throw less than its function type allows. A closure used as a
  value of another function type (one taking more parameters, the other arm of a `? :`) still
  throws only what it throws, and so does a call through a `const` holding a closure.
- Interface methods and overridden methods share one error type per method: the interface's (or
  base method's) `throws` clause bounds every implementation
  (``` `Db.get` throws `Forbidden`, but `Store.get` does not allow it ```); without one it is the
  union of what the implementations throw. For an interface method returning a promise it is
  what the promise rejects with ([Async](async.md#errors)), also for the async methods of an
  object literal implementing the interface (`const l: Loader = { async load(s) { … } }`).
- **Higher-order functions** propagate their callback's errors by being generic over them:
  `function run<E>(f: () => i64 throws E): i64 throws E`. The prelude's array methods
  (`forEach`, `map`, `filter`, `reduce`, `find`, `findIndex`, `some`, `every`), `Map` methods
  (`forEach`, `upsert`, `update`, `getOrInsert`), `Array.from`'s callback and `(T | null).map`
  do this, so
  `xs.map((x) => parse(x))` throws what `parse` throws, and nothing when the callback cannot
  throw. Sort comparators and `Mutex.with` callbacks cannot throw.

## Errors as values

Return a union (`function find(id: string): User | NotFound`) and narrow it, or turn a throwing
call into a value with `attempt(() => f())`: its type is `T | E` (`E | null` when `f` returns
nothing; `null` means success).

## Uncaught errors and panics

- An error escaping `main` prints `Uncaught <header> at file:line:col` to stderr and exits with
  code 1. `<header>` is the first line of Node's report: `<name>: <message>` as `Error`'s
  `toString()` writes it (`Uncaught AppError: boom`), with the error's actual class in front
  when its `name` ends in `Error` but is not the class's name (`class E extends TypeError {}`
  gives `E [TypeError]: boom`; `MyTypeError: boom` when the class's name contains the name).
  The class is the dynamic one (an `IoError` thrown through a function declared `throws Error`
  reports `IoError`), and the location is where it was thrown, also when a `finally` it leaves
  through throws and catches another error; for an error thrown inside the standard library, it is the line of your code that called
  into it. This holds for `async function main()` too, and for an error a spawned task threw
  that `main` awaited. A thrown value that is not an `Error` is written as Node writes it, the
  way `console.log` prints it: `throw "too big"` reports `Uncaught too big`, `throw 42`
  `Uncaught 42` (a number as `String(x)` writes it: `throw -0` reports `Uncaught 0`), and an
  object `Uncaught { code: 503 }`. `JSON.parse`'s `JsonError` reports
  `Uncaught SyntaxError: <message>`, as Node reports the error `JSON.parse` throws. An `Error`
  held in an `any` or `unknown` (`const e: any = new RangeError("x"); throw e;`, or passed to a
  generator's `throw()`) gets the `Error` header too (`Uncaught RangeError: x`). Only the
  first uncaught error is reported, also when tasks running in parallel fail at once
  ([Async](async.md)).
- Node's report shows where an `Error` was **created** (the top of its stack); Velt's location
  is where it was last thrown. They differ for a rethrow (`catch (e) { throw e; }`) and for an
  error made earlier than it is thrown (#356).
- **Panics** are bugs, not errors: an index out of bounds, integer division by zero,
  `panic(msg)`. They print `panic: … at file:line:col` and exit with code 101. They cannot be
  caught. A failed `assert`, `assertEq` or `assertThrows` throws an `AssertionError` (an
  `Error`, as Node's `assert` throws), which can. Like any error, it is part of the inferred
  error type of the function that calls them: a function with `throws ParseError` that calls
  `assert` must allow `AssertionError` too (`throws ParseError | AssertionError`), and a `catch`
  that handles only `ParseError` no longer covers everything the block throws.
- There is no `Result` type, no `Ok`/`Err` and no postfix `?`: errors propagate by themselves,
  and values use unions.

## Inference limits

A closure created inside a recursive function that it calls throws what the function throws,
also what it throws after the closure (`xs.map((x) => walk(x, d + 1))`, then `throw new
TypeError(…)`), and so does the generator a recursive generator function returns
(`yield* walk(child)` before a `throw`), and a `catch` or promise whose error type depends on a
function still being checked through recursion: the program is checked again with the error
types the recursion found, as in Node, and needs no `throws` clause. Only an error type the
next check cannot carry over (an object type, `throw { code: 1 }`) still needs one (``` the
error type of this function is not known yet ```). A call through an interface
value whose method's error type is found only after the call is checked (the recursion goes
through the interface, or an object literal is converted to the interface by code checked
later) needs none, also when the `catch` or promise sees it through other functions (an
`async` function that reads the interface value after an `await`): the program is checked
again with that error type known, as in Node. Only when it holds a type the next check
cannot carry over (an object type, `throw { code: 1 }`) does the `throws` clause go on the
interface method (`get size(): string throws { code: string };`). Error types of overridden
methods, and inferred error types of interface methods, cannot depend on type parameters; a
written `throws` clause of a generic interface's method can name the interface's own parameters
(`next(): IteratorResult<T> throws E` in `Iterator<T, E>`): an implementation
then throws what its `implements Iterator<string, IoError>` says, and a call through an
`Iterator<string, E>` value throws `E` (also through an interface extending it with its
parameters in another order). A method of another interface without a `throws` clause that
such implementations also implement (`class C implements Iterator<string, IoError>, Pull` with
`Pull.next()`) throws what they throw, which must then be the same type for all of them
(``` the implementations of `Pull.next` throw different errors ```).

## Example

```ts
class NotFound extends Error {
  id: string;

  constructor(id: string) {
    super(`no user ${id}`);
    this.id = id;
  }
}

class Forbidden extends Error {}

function load(id: string): string throws NotFound | Forbidden {
  if (id == "0") {
    throw new Forbidden("admins only");
  }
  if (id != "42") {
    throw new NotFound(id);
  }
  return "ada";
}

function describe(id: string): string {
  try {
    return `user ${load(id)}`;
  } catch (e) {                         // e: NotFound | Forbidden
    if (e instanceof NotFound) {
      return `missing ${e.id}`;
    } else if (e instanceof Forbidden) {
      return `denied: ${e.message}`;
    }                                   // every member handled: no return needed here
  }
}

function lengths(ids: string[]): number[] {
  return ids.map((id) => load(id).length);  // throws what `load` throws
}

try {
  console.log(describe("42"), describe("7"), describe("0"));
  console.log(lengths(["42", "1"]));
} catch (e) {
  console.log("lengths failed:", e.message);
} finally {
  console.log("done");
}
const r = attempt(() => load("7"));         // string | NotFound | Forbidden
console.log(r instanceof NotFound ? `absent ${r.id}` : "found or denied");
```

Async errors (rejections, `Promise.all`, spawned tasks) are covered in
[Async and concurrency](async.md#errors).
