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
3. **Add something only where TypeScript can't express it at native speed**: integer types,
   `shared` state across threads, `extend`.

## Programs are compiled

- A program starts at `function main()` or `async function main()` in its root file. `main`
  may return an `i32` exit code.
- Module scope holds declarations only: functions, classes, types, imports and constants. Top-
  level statements go in `main`, and a module-level `let` is an error ("mutable module-level
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

The difference: integer types (`i8` … `i64`, `u8` … `u64`, `isize`, `usize`) exist, and you
opt into them by writing them. Declared integers do integer arithmetic: `/` truncates when both
sides are declared integers, values wrap at their width instead of losing precision past 2^53,
and integer division by zero panics. Lengths and indexes are `usize`. *Why*: integer loops and
indexes run at integer speed, and you decide where integer semantics apply
([Numbers](../reference/types.md#numbers)).

Declared types don't convert implicitly; `as` converts between number types:

```ts
const len = [1, 2, 3].length;     // usize
const half = len as f64 / 2.0;    // 1.5
console.log(half, 300 as u8);     // 1.5 44 (integers wrap)
```

## Strings

- **No implicit conversion**: `"Total: " + 5` and `"a" + true` are compile errors; use a
  template literal, `` `Total: ${n}` ``. *Why*: `"5" + 1 === "51"` and
  `"Total: " + a + b` bugs can't happen.
- **Lengths and positions are in bytes** of UTF-8, not UTF-16 code units: `"héllo".length` is
  6. `slice`, `indexOf` and regex offsets are byte offsets. There is no `s[i]` and no
  `for...of` over a string; use `slice`, `split("")` or `charCodeAt`. *Why*: strings are UTF-8
  throughout, so no conversion is ever needed.
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
  reference; build a `User` from its fields), and an interface that refers to itself through a
  field (`next?: Node`) needs an array or a class (#376).
- Interfaces may have **default method bodies**. `extend` adds methods to any type, including
  `string`, arrays and your unions.
- `as` converts numbers only; there are no type assertions. Narrow with `typeof`, `instanceof`,
  `==` or a discriminant instead.
- Enums are numeric or string enums; tagged data is a discriminated union (payload enums and
  `match` don't exist).
- Not available: `keyof`, mapped and conditional types, template literal types, utility types
  (`Partial`, `Pick`, …), index signatures, declaration merging, `namespace`.

## Classes

- Single inheritance; `override` is required on redefined methods; there are no abstract
  classes and no `protected` members (`private` is private to the declaring class); a
  constructor can be `private` or `protected`, with TypeScript's rules.
- `static readonly` constants exist; mutable statics don't.
- `new` evaluates all field initializers (base class first) before running the constructors;
  TypeScript runs the base initializers and constructor before the derived initializers. A known
  difference, tracked in [#273](https://github.com/velt-lang/velt/issues/273).
- A method that is never overridden is called directly; only overridden methods use a vtable.
- `struct` declares an object type with the same members as a class, built from a literal
  (no constructor). **Planned**
  ([semantics](../internals/design/semantics.md#js-fidelity-decisions)): `struct` goes away in
  favor of `class` and `type Name = { … }` with `extend`.

## Functions

- No `function` expressions (use arrows), no `this` rebinding, no `arguments`.
- No rest parameters, no spread arguments (`f(...xs)`), no overloads. Optional and default
  parameters work.
- Parameter types are required; the return type is inferred only as `void` when omitted.
- Generics are compiled per instantiation (monomorphized), so generic code is as fast as
  hand-written code. Bounds are interfaces.

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
  and `reject` may be kept and called later from any task. No global `setTimeout` (use
  `sleep(ms)` or [`velt:timers`](../std/timers.md)), no `for await`, no async generators.

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
  through its `index.vlt`. `paths` aliases in `package.vlt` work like `compilerOptions.paths`.

## Not supported

`var`, `eval`, prototypes, `delete` (other than on a `Record`), `for...in`, `with`, getters on object literals,
decorators, generators (`function*`, `yield`), `Symbol` (other than `Symbol.dispose` and
`Symbol.asyncDispose`), `BigInt` literals (use [`velt:bigint`](../std/bigint.md)), Unicode
identifiers, and the logical assignments `&&=`, `||=`, `??=` (planned). JSX is supported for
server-side rendering ([`velt:jsx`](../std/jsx.md)); components can't take `children` yet.

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
| `Promise<T>` rejects with anything | `Promise<T, E>` carries its rejection type | — |
| floating promises lose errors | a floating promise is a compile error | — |
| `new Promise(...)` | same, with an arrow-function executor; `await` of one abandoned unsettled is reported | `new Promise(...)` |
| single-threaded event loop | multi-core runtime; `spawn`, `shared`, `Mutex`; data races are compile errors | — |
| mutable module globals | constants only | — |
| `arr.sort()` sorts as strings | `sort()` and `toSorted()` sort numbers numerically; with a comparator they work like TypeScript | — |
| `xs.sort()`, `xs.reverse()`, `xs.fill(v)` return the array | they work in place and return nothing (returning the array would make it reference counted); `xs.toSorted()` and `xs.toReversed()` return sorted / reversed copies, as in ES2023 | — |
| `xs.length = 0` | `xs.truncate(0)`; `length` is read-only (arrays have no holes) | — |
| `xs.splice(i, n, a, b)`, `xs.push(a, b)` | `splice(i, n)` removes; one `push(x)` per element | inserting `splice` and `push` with rest parameters |
| `p.then(f).catch(g)` | `await p` inside `try`/`catch` | — |
| `process.stdout.write(s)`, `process.stderr.write(s)`, `process.env.X` | the same on the builtin `process` (`process.env.X` is `string \| null`, with no `undefined`, so `process.env.NOPE !== null` is `false`; assigning to it is `setEnv` and `delete` is `removeEnv`, from `velt:process`) | — |
| `Object.keys(process.env)`, `{ ...process.env }` | `envAll()` from `velt:process`: a `Record<string, string>` snapshot, in the same order | — |
| `process.argv` | `args()` from `velt:process`: the arguments after the program, like `process.argv.slice(2)` | — |
| `a.localeCompare(b, locale, options)` (the host's locale by default) | `a.localeCompare(b)`: the CLDR root collation, like `new Intl.Collator("und").compare(a, b)`; no locales | — |
| `export default` | named exports only | — |
| string length in UTF-16 units | length and offsets in UTF-8 bytes | — |
| (no equivalent) | `extend` adds members to any type | module-scoped extensions, retroactive `implements` |
| JSX | server-side rendering through a `jsxImportSource` provider ([`velt:jsx`](../std/jsx.md)) | no client-side DOM; see [TSX](../internals/design/tsx.md) for what is planned |
