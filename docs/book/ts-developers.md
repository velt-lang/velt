# Velt for TypeScript developers

If you write TypeScript, you already write most of Velt: classes, interfaces, generics, unions
and narrowing, discriminated unions, closures, `async`/`await`, template literals,
destructuring, spread, optional chaining, `??`, ES modules, `throw`/`try`/`catch`, `using`.
This page lists **every place where Velt differs**, and why.

The differences come from three rules:

1. **Keep TypeScript's syntax and meaning wherever native speed allows it.** When Velt runs
   your code, it should do what TypeScript would.
2. **Drop the JavaScript behavior that causes bugs**, even when it means ported code must
   change. The compiler then says exactly what to write instead, and editors offer it as a
   quick fix.
3. **Add what makes native code faster.** Velt compiles to machine code, not to JavaScript, so
   it isn't limited to what TypeScript can express. Integer types, `shared` state across threads
   and `extend` are opt-ins to reach for when you want more speed; code without them still runs
   as TypeScript would.

## Programs are compiled

- A program starts at its root file's top-level statements, as a TS file does, or at
  `function main()` / `async function main()` (which may return an `i32` exit code). Top-level
  statements run in a generated `main`; only the root file may have them
  ([Scripts](../reference/modules.md#scripts-top-level-statements)).
- Module scope holds no mutable state: a top-level variable that functions use stays a module
  constant, and a module-level `let` that a function uses is an error ("mutable module-level
  state is not allowed"). *Why*: no hidden global state means request handlers can't race on
  it, and `velt dev` can hot-swap code without migrating globals.
- Types are checked once, at compile time, and then gone: there are no runtime type checks,
  no `any`, no `unknown`. Dynamic JSON is a `JsonValue`.
- Semicolons are required (no automatic semicolon insertion).

## Numbers

`number` is `f64`, and number literals behave like JavaScript numbers:

```ts
const a = 7;
console.log(a / 2, 0.1 + 0.2);    // 3.5 0.30000000000000004
```

Numbers the standard library gives you behave the same: lengths, `indexOf`, `size`, indexes.
`for (let i = 0; i < xs.length; i++)` works, `xs.length / 2` is `1.5` for three elements, and
`xs[i]` takes a `number` (a non-whole index panics).

The difference: integer types (`i8` … `i64`, `u8` … `u64`, `isize`, `usize`) exist, and you
opt into them by writing them. Declared integers do integer arithmetic: `/` truncates when both
sides are declared integers, values wrap at their width instead of losing precision past 2^53,
and integer division by zero panics. *Why*: integer loops and indexes run at integer speed
either way (numbers that hold whole values are stored as integers), and you decide where
integer semantics apply ([Numbers](../reference/types.md#numbers)).

Declared types don't convert implicitly; `as` converts between number types:

```ts
const xs = [1, 2, 3];
console.log(xs.length / 2);       // 1.5
const n: i64 = 7;
console.log(n / 2, n as f64 / 2, 300 as u8); // 3 3.5 44 (integers wrap)
```

## Booleans

`boolean` works as in TypeScript. Velt also accepts the shorter `bool` for the same type, so
`(x: bool) => boolean` and `boolean[]` mix freely. Write `boolean` in code that `tsc` must also
accept; compiler messages and editors print `boolean` either way
([Booleans](../reference/types.md#booleans)).

## Strings

- **No implicit conversion**: `"Total: " + 5` and `"a" + true` are compile errors; use a
  template literal, `` `Total: ${n}` ``. *Why*: `"5" + 1 === "51"` and
  `"Total: " + a + b` bugs can't happen.
- Lengths and positions count UTF-16 code units, as in JS (`"😀".length` is 2), and `<` orders
  by code units. `s[i]` is `s.charAt(i)`, but `""` past the end where JS gives `undefined`, and
  `charCodeAt` out of range is `-1` where JS gives `NaN`. Text is stored as UTF-8, so files,
  sockets and HTTP bodies need no conversion; `Buffer.byteLength(s)` is the UTF-8 size.
- Strings are immutable values, as in JS, and cheap to copy.

## `null`, not `undefined`

There is one "nothing": `null`. `undefined` is a compile error with the fix "use `null`", and
`a?: T` means `T | null`:

```ts
class Config {
  host: string = "localhost";
  port?: i64;                       // T | null, starts as null
}

function connect(c: Config, timeout?: i64): string {
  return `${c.host}:${c.port ?? 80} (${timeout ?? 30} s)`;
}

console.log(connect(new Config()));  // localhost:80 (30 s)
```

*Why*: the `null` vs `undefined` bug class disappears. `JSON.parse` treats an absent key like
`null`; only `JsonValue` tells them apart.

## Truthiness

Conditions, `!`, `&&` and `||` take `bool` and nullable values. A nullable is true when it is
not null, so `if (!user) return;` is a null check and narrows `user`. Numbers and strings are
rejected:

```ts error
function main() {
  const count = 0;
  const port: i64 | null = null;
  if (count) {                       // error: write `count !== 0`
    console.log(port || 8080);       // error: use `??` for a default
  }
}
```

*Why*: `0`, `""` and `NaN` being false is the source of the `port || 8080` and
`if (items.length)` class of bugs. Each error names the comparison to write.

## Equality

`==` and `===` are the same operator (as are `!=` and `!==`), and both sides must have the same
type: `1 == "1"` is a compile error. Strings compare by content; every object (arrays, class
instances, structs, object literals) compares by identity, as in JS. `deepEqual(a, b)` compares
contents.

## Objects and types

- **Object types are exact**: an object literal can't have extra fields, and you can't add a
  property later. Use a `Map` for dynamic keys. *Why*: every object has a fixed layout, so a
  field access is one load.
- **Interfaces with methods are nominal**: a class implements one by declaring `implements`,
  and an object literal does not satisfy one. An **interface with only fields** is an object
  type, like `type User = { … }`, so model interfaces work as in TypeScript: literals satisfy
  them, `JSON.parse<User>` reads them, and as a bound (`<T extends HasId>`) any type with the
  fields fits. Unlike TypeScript, a class instance is not a `User` value (it is shared by
  reference; build a `User` from its fields). Recursive models (`next?: Node`) work, and
  `JSON.stringify` of one that contains itself fails as in JavaScript.
- Interfaces may have **default method bodies**. `extend` adds methods to any type, including
  `string`, arrays and your unions.
- `as` converts numbers and brands a value (`"u1" as UserId`, below); there are no other type
  assertions. Narrow with `typeof`, `instanceof`, `==` or a discriminant instead.
- Enums are numeric or string enums; tagged data is a discriminated union (payload enums and
  `match` don't exist).
- `Partial`, `Required`, `Readonly`, `Pick` and `Omit` work on concrete object types, also through
  generic aliases like `type WithoutChildren<P> = Omit<P, "children">` (not yet on a type
  parameter inside a generic function, #350). `Pick` rejects a key that isn't a field (`Omit`
  warns).
- Intersections `A & B` of object types work as in TypeScript, unions distributing over them,
  and so do indexed access types (`User["name"]`) and branded primitives
  (`type UserId = string & { __brand: "UserId" }`, zero-cost). Parts with no value in common
  are an error instead of `never`; classes, type parameters (#350) and function types
  (overloads) can't be parts; `A & B` doesn't convert to `A` without a copy (`{ ...ab }`)
  ([Intersection types](../reference/types.md#intersection-types)).
- Not available: `keyof`, mapped and conditional types, template literal types, the other
  utility types (`Record` aside), index signatures, declaration merging, `namespace`.

## Classes

- Single inheritance; `override` is required on redefined methods; there are no abstract
  classes and no `protected` members (`private` is private to the declaring class); a
  constructor can be `private` or `protected`, with TypeScript's rules. ES private names
  (`#x`, `#m()`, `#x in o`) work as in JavaScript: hidden from `console.log`, `JSON` and
  `Object.keys`, never inherited.
- `static readonly` constants exist; mutable statics don't.
- Constructors follow TypeScript's `super(...)` rules: a derived constructor calls it exactly
  once (also when the base has no constructor), and statements before it cannot use `this`.
  As in TypeScript 4.6+, such statements are allowed also when the class has initialized fields
  or parameter properties; those are set right after `super(...)` returns.
- Field initializers and constructors run in JavaScript's order (base initializers, base
  constructor, derived initializers, derived constructor), and parameter properties come first
  in the field order, as `tsc --target es2022` emits them.
- A method that is never overridden is called directly; only overridden methods use a vtable.
- `struct` declares an object type with the same members as a class, built from a literal
  (no constructor). **Planned**
  ([semantics](../internals/design/semantics.md#js-fidelity-decisions)): `struct` goes away in
  favor of `class` and `type Name = { … }` with `extend`.

## Functions

- No `function` expressions except generators (`const g = function* () { … }`; otherwise use
  arrows), no `this` rebinding, no `arguments`.
- No overloads. Optional and default parameters work, on arrows too; rest parameters
  (`...xs: T[]`) take spread arguments (`f(...xs)`) at their position. Callbacks may take fewer
  parameters than they are passed (`xs.map((x) => …)` gets `(x, i)`).
- Parameter types are required. As in TypeScript, an omitted return type is inferred from the
  `return` expressions (a union when they differ, `Promise<T>` for `async`, `void` without a
  value). As in TypeScript, a function whose `return` expressions depend on the function
  itself needs an annotation; uses elsewhere in the body don't. A `return;` next
  to `return value;` is an error rather than `T | undefined`: return `null` with a `T | null`
  type.
- Generics are compiled per instantiation (monomorphized), so generic code is as fast as
  hand-written code. Bounds are interfaces.
- Generators (`function*`, `*name()` methods, `yield`, `yield*`) work as in JS, lazily, with
  their return type written (`Generator<T>`, `Iterable<T>`, `Iterator<T>`,
  `IterableIterator<T>` or `IteratorObject<T>`). A `for...of` over
  a generator call allocates nothing and runs like a hand-written loop. There is no `return
  value`, `next(value)` or `throw()`, and a `finally` block in a generator cannot `yield`,
  throw, or `break` out of it ([Generators](../reference/functions.md#generators)).
- Async generators (`async function*`, `async *name()`) and `for await` work as in JS too
  (`AsyncGenerator<T>`); a `for await` over an async generator call allocates nothing
  ([Async generators](../reference/functions.md#async-generators)). Unlike JS, dropping a
  generator closes it (its `finally` blocks run), calling `next()` or `return()` on a generator
  from inside its own body stops the program (`generator is already running`; JS throws a
  catchable `TypeError`), and a generator cannot be passed to another task.

## Errors

`throw`, `try`, `catch` and `finally` look the same, but errors are typed and checked:

- `catch (e)` gives `e` the exact union of what the `try` block can throw, never `unknown`.
- Functions may declare `throws A | B`; the compiler infers it otherwise, and checks it when
  written. Callbacks propagate errors: `xs.map((x) => parse(x))` throws what `parse` throws.
- `attempt(() => f())` turns a throwing call into a value: `T | E`.
- Index out of bounds, integer division by zero and failed assertions are **panics**: bugs that
  stop the program (exit code 101) and can't be caught.

*Why*: no unwinding (each throwing call is a cheap check), and no error type is ever a
surprise ([Error handling](errors.md)).

## Async

- Promises start when created, like JS, and a directly awaited call costs nothing.
- **A promise that is neither awaited nor spawned is a compile error** ("floating promise").
  *Why*: a forgotten `await` silently loses errors in JS.
- `spawn(f())` runs a task on another core; the runtime is multi-threaded. Data shared between
  tasks must be `shared(...)` or a `Mutex`, and data races are compile errors.
- A promise's type carries its error type: `Promise<T, E>`.
- Promises have no `then`, `catch` or `finally`: `await` them, inside `try`/`catch`/`finally`
  to handle their errors. *Why*: one way to sequence async code, and errors stay typed.
- `new Promise((resolve, reject) => …)` and `Promise.withResolvers()` work as in JS; `resolve`
  and `reject` may be kept and called later from any task. `setTimeout`, `setInterval` and
  their `clear` functions are globals; the callback returns the promise to run
  (`setTimeout(() => save(doc), 100)` or `async () => { … }`), and a pending timer keeps the
  process alive unless it is `unref()`ed, as in Node ([`velt:timers`](../std/timers.md)). To wait, `await sleep(ms)`.
- A promise has one owner (for now; shared promises are planned in #212). `const q = p` moves
  it, so using `p` afterwards is an error, and a promise can't be copied out of a collection:
  `arr[i]` moved or bound (`const p = arr[0]`), `[...arr]`, `const [a, b] = arr`, `m.get(k)`,
  `Object.values(r)` and generic code that copies its elements report an error that says
  TypeScript allows it and names the alternative. Take promises out with `pop()` or
  `splice(i, 1)`, await them together with `Promise.all(arr)`, or store the awaited results.
  Replacing one in place (`arr[i] = p`) works, and so does reading a class instance that holds
  one (`m.get(k)` of a `Map<string, Job>`). Printing a promise is not supported yet (#413).
- Std streams are async iterables: `for await` over a [channel](../std/channel.md), a file's
  `lines()`, standard input's `lines()`, a WebSocket, a Redis subscriber or a `Ticker`. Unlike
  a Node stream, whose iterator destroys the stream when the loop is left early, leaving the
  loop keeps the source open (a channel may have other receivers): close it yourself
  ([Std sources](../reference/async.md#std-sources)).

## Memory

There is no garbage collector, so no GC pauses and no heap tuning: memory is freed as soon as
its last reference goes away, and `[Symbol.dispose]()` runs at that moment. You never write
lifetimes, borrows or `mut`; the compiler infers them. Objects are shared references exactly
like in JS (`const b = a; b.push(1)` changes `a`), and `.clone()` is an explicit deep copy.
Values with a single owner cost nothing extra; only types the program actually shares get a
reference count. Reference cycles are not freed (**planned**: `weak` references)
([Memory without a garbage collector](memory.md)).

## Modules

- Named exports only: `export default` and default imports are errors with a fix.
- Standard library modules use the `velt:` prefix: `import { readFile } from "velt:fs"`.
- Relative imports drop the extension: `import { x } from "./util"`. A folder is a module
  through its `index.vlt`, `index.ts` or `index.tsx`. `paths` aliases in `package.vlt` work like `compilerOptions.paths`.
- Modules can be `.ts` and `.tsx` files as well as `.vlt`, so a folder can be shared with a
  TypeScript project; as in TypeScript, JSX needs `.tsx`, and `"./x.js"` names `x.ts`
  ([TypeScript files](../reference/modules.md#typescript-files-ts-and-tsx)).

## Not supported

`var`, `eval`, prototypes, `delete` (other than on a `Record`), `for...in`, `with`, getters on object literals,
decorators, `Symbol` (other
than `Symbol.dispose`, `Symbol.asyncDispose`, `Symbol.iterator` and `Symbol.asyncIterator` as
method names), `BigInt` literals (use [`velt:bigint`](../std/bigint.md)), Unicode
identifiers. `x!` is checked (a `null` panics) where TypeScript trusts it, and `as const` keeps
the value as it is. `Date` follows JS (months 0-11, local-time getters); its `toString()` has no
time zone name and its `toLocale…` methods always format as `en-US`. JSX is supported for
server-side rendering ([TSX](../reference/tsx.md), [`velt:jsx`](../std/jsx.md)).

## Quick reference

| TypeScript / JavaScript | Velt today | Coming |
|---|---|---|
| `number` is always a float | `number` is `f64`; integer literals are stored as integers but `/` still gives `3.5`; `i64`, `u8`, … are opt-in | — |
| `"5" + 1 === "51"` | compile error: use a template literal | — |
| `null` and `undefined` | `null` only; `a?: T` is `T \| null` | — |
| `if (count)`, `port \|\| 8080` | conditions take `bool` and nullable values; `??` for defaults | — |
| `==` coerces | `==` is `===` (objects by identity, `deepEqual` for contents); both sides have the same type | — |
| objects are shared references | the same: arrays, maps, class instances, object types and closures are references, freed when the last reference goes | — |
| garbage collector | deterministic freeing, no pauses; `[Symbol.dispose]()`, `using`, `await using` | `weak` references (stage 3) |
| structural typing everywhere | object types and interfaces with only fields structural but exact; interfaces with methods nominal | — |
| `any`, `unknown`, type assertions | none; `as` converts numbers; `JsonValue` for dynamic data | — |
| `catch (e: unknown)` | `e` is the exact union of what the `try` can throw | — |
| `const p = promises[0]`, `m.get(k)` on promises (several holders of one promise) | a promise has one owner: `pop()`, `splice`, `Promise.all(arr)`; shared promises are planned (#212) | `promises.pop()` |
| `Promise<T>` rejects with anything | `Promise<T, E>` carries its rejection type | — |
| floating promises lose errors | a floating promise is a compile error | — |
| an unhandled rejection prints the source line and a stack, and is reported when no handler is attached by the end of the turn | prints `Uncaught <Type>: <message> at file:line:col` and exits with code 1, reported only once nothing can await the promise any more: `const h = spawn(f()); await sleep(100); await h;` handles `h`'s rejection | — |
| `new Promise(...)` | same, with an arrow-function executor; `await` of one abandoned unsettled is reported | `new Promise(...)` |
| single-threaded event loop | multi-core runtime; `spawn`, `shared`, `Mutex`; data races are compile errors | — |
| top-level statements | run in a generated `main` (root file only) | — |
| mutable module globals | constants only | — |
| `arr.sort()` sorts as strings | `sort()` and `toSorted()` sort numbers numerically; with a comparator they work like TypeScript | — |
| `xs.sort()`, `xs.reverse()`, `xs.fill(v)` return the array | they work in place and return nothing (returning the array would make it reference counted), but a chained access reads the changed array as in TypeScript (`xs.sort().join(",")`); other uses of the result are an error with the fix; `xs.toSorted()` and `xs.toReversed()` return sorted / reversed copies, as in ES2023 | — |
| `xs.length = 0` | `xs.truncate(0)`; `length` is read-only (arrays have no holes) | — |
| `xs.splice(i, n, a, b)`, `xs.push(a, b)` | `splice(i, n)` removes; one `push(x)` per element | inserting `splice` and `push` with rest parameters |
| `p.then(f).catch(g)` | `await p` inside `try`/`catch` | — |
| `process.stdout.write(s)`, `process.stderr.write(s)`, `process.env.X` | the same on the builtin `process` (`process.env.X` is `string \| null`, with no `undefined`, so `process.env.NOPE !== null` is `false`; assigning to it is `setEnv` and `delete` is `removeEnv`, from `velt:process`) | — |
| `Object.keys(process.env)`, `{ ...process.env }` | `envAll()` from `velt:process`: a `Record<string, string>` snapshot, in the same order | — |
| `process.argv` | the same layout, `[runtime, script, ...args]`: the script is the source file under `velt run`, the executable for a built program; each read is a new array, so changing it in place is an error (copy it first) | — |
| `a.localeCompare(b, locale, options)` (the host's locale by default) | `a.localeCompare(b)`: the CLDR root collation, like `new Intl.Collator("und").compare(a, b)`; no locales | — |
| `export default` | named exports only | — |
| `for...of` over any `Iterable`; `IteratorResult` has `value: undefined` when done | the same protocol (`[Symbol.iterator]()`, `next()`, `return()` on early exit, returning `{ done: true }`); a done result has no `value` (unnarrowed, `r.value` is `T \| null`, so `g().next().value` works); `Iterator<T, E>` carries the error type `next()` throws; `for await` over `AsyncIterable`s, and over arrays of promises | — |
| generators: `function*`, `yield`, `yield*`, `Generator<T, TReturn, TNext>` | the same, lazy, `Generator<T, E>` (`E`: what the body throws); TS's `Generator<T, void, unknown>` spelling means `Generator<T>`, and a real `TReturn` is an error; no `return value`, `next(value)` (so `yield` has no value) or `throw()`, each an error that says so; a `for...of` over a call allocates nothing; async generators (`AsyncGenerator<T, E>`) likewise, and `yield p` there awaits a promise `p` as in JS | — |
| arrays, strings, `Map`s and `Set`s are `Iterable`; `a[Symbol.iterator]()` | the same: they convert to `Iterable<T>` values and satisfy `Iterable<T>` bounds (`sum(xs: Iterable<number>)` takes `[1, 2, 3]`); `x[Symbol.iterator]()` returns an `Iterator<T>`, live for arrays as in JS; map and set iterators see the entries as of the call (JS's are live), and `m.keys()` / `values()` / `entries()` are arrays | — |
| `IterableIterator<T>`, `IteratorObject<T>`, `AsyncIterableIterator<T>`, `IteratorResult<T, TReturn>` | the same interfaces (`[Symbol.iterator]()` returns `Iterator<T>`: no covariant returns); generators implement them, and such a value converts to an `Iterable<T>`. `IteratorResult<T, void>` is `IteratorResult<T>`; a real `TReturn` is an error | an `IterableIterator<T>` value converting to `Iterator<T>` (interface values don't convert to the interfaces they extend) |
| spread, `Array.from`, array destructuring, `new Map` / `new Set` of any iterable (strings, `Map`s, `Set`s, generators) | the same: an iterable is iterated where it stands, destructuring takes only the values it needs and then closes the iterator, and over a direct generator call only the result is allocated | an array-typed spread source is evaluated before the literal's other elements (`[f(), ...g()]` with `g(): T[]` calls `g` first; the elements are still read in place); a nested pattern takes its values after the outer one has taken all of its own |
| function expressions: `function* () {}`, `async function* () {}`, `function () {}` | generator expressions work, sharing the variables they use as in JS (no recursion through the expression's name); other function expressions are arrows | — |
| object literals with methods: `{ *[Symbol.iterator]() { ... } }`, `{ m() { ... } }` | an object literal whose one member is an iterator method is an `Iterable<T>` (no `this`); other methods are errors | — |
| an unreachable generator is never closed: its `finally` blocks never run | dropping the last reference to a suspended generator closes it: its `finally` blocks run and its `using` values are disposed then (there is no garbage collector to wait for) | — |
| `next()` on a generator from inside its own body throws a catchable `TypeError` | it panics (`generator is already running`) | — |
| `return` / `yield`, a line break, then an expression: automatic semicolon insertion ends the statement after `return` / `yield` | no automatic semicolon insertion: the expression on the next line is returned / yielded | — |
| (no equivalent) | `extend` adds members to any type | module-scoped extensions, retroactive `implements` |
| JSX | server-side rendering through a `jsxImportSource` provider ([`velt:jsx`](../std/jsx.md)) | no client-side DOM; the differences are in [the reference](../reference/tsx.md#differences-from-typescript) |
