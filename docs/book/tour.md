# A tour of Velt

Velt reads like TypeScript and runs like Rust: it compiles to native code, has no garbage
collector, and runs async code on a multi-core runtime. This tour shows the language in one
sitting. Every example compiles; the full rules are in [the Reference](../reference/README.md).

## Values and types

```ts
const a = 7;             // an integer (stored as i64) that behaves like a JS number
const x: f64 = 1.5;      // number is f64
console.log(a / 2, a + x);  // 3.5 8.5, like JS
const n: i64 = 7;        // a declared integer type: integer math, opt-in for speed
console.log(n / 2, Math.trunc(a / 2));   // 3 3: integer division
let small: u8 = 250;
small += 10;             // 4: integer math wraps
console.log(n as f64 / 2.0, 300 as u8);  // 3.5 44: declared types convert only with `as`
const s = `a=${a} x=${x}`;               // template literals; "a=" + a is a compile error
```

Types are required on function signatures and inferred everywhere else. Conditions take `bool`
and nullable values (`if (!user) return;` is a null check), never numbers or strings (write
`if (count !== 0)`, not `if (count)`), and `null` is the only "nothing": there is no
`undefined`. See [Types](../reference/types.md) and
[Variables and conditions](../reference/variables.md).

## Functions and errors

```ts
function fib(n: i64): i64 {
  return n < 2 ? n : fib(n - 1) + fib(n - 2);
}

class ParseError extends Error {}
class Overflow extends Error {}

function digit(s: string): i64 throws ParseError {   // `throws` is optional (inferred)
  if (s == "1") return 1;
  throw new ParseError(`bad digit: ${s}`);   // compiled to a result return: no unwinding
}

function sum(xs: string[]): i64 {
  const total = xs.map((x) => digit(x)).reduce((a, b) => a + b, 0);
  if (total > 9) throw new Overflow("too big");
  return total;                             // throws ParseError | Overflow (inferred)
}

function main() {
  try {
    console.log(fib(10), sum(["1", "1"]), sum(["1", "x"]));
  } catch (e) {                             // e: ParseError | Overflow
    if (e instanceof ParseError) console.log("parse:", e.message);
    else console.log("overflow");
  }
  const r = attempt(() => digit("7"));      // i64 | ParseError: the error as a value
  console.log(r instanceof ParseError ? "no digit" : `digit ${r}`);
}
```

Errors are typed: `catch (e)` gets the exact union of what the `try` block can throw, calls
propagate errors automatically (also out of callbacks like `map`), and a `throws` clause is
checked when you write one ([Error handling](errors.md)).

## Classes, interfaces and structs

```ts
struct Point {                      // a value type: copied, stored inline
  x: f64;
  y: f64;
  len(): f64 { return Math.sqrt(this.x * this.x + this.y * this.y); }
}

interface Named {
  name(): string;
  greet(): string { return `hi ${this.name()}`; }   // a default method
}

class Animal implements Named {     // a heap object with one owner
  readonly species: string;
  private id: i64 = 0;
  constructor(species: string) { this.species = species; }
  name(): string { return this.species; }
  speak(): string { return "..."; }
}

class Dog extends Animal {
  override speak(): string { return "woof"; }        // virtual only because it's overridden
}

function greetAll<T extends Named>(xs: T[]): string[] {   // monomorphized: static dispatch
  return xs.map((x) => x.greet());
}

function main() {
  const pets: Named[] = [new Dog("rex"), new Animal("cat")];  // interface values: dynamic dispatch
  for (const p of pets) console.log(p.greet());
  console.log(greetAll([new Dog("fido")]), Point { x: 3.0, y: 4.0 }.len());
}
```

Classes support `private`, `readonly`, getters and setters, `static` members, constructor
parameter properties, single `extends` and any number of `implements`. Generics are
monomorphized. `extend` adds methods to existing types, builtins included
([Classes](../reference/classes.md)).

## Discriminated unions and `switch`

Tagged data is a discriminated union (object types sharing a literal-typed field), and `switch`
takes it apart, narrowing the value in each case:

```ts
type Shape = { kind: "circle"; r: f64 } | { kind: "rect"; w: f64; h: f64 };

function area(s: Shape): f64 {
  switch (s.kind) {                          // a jump table on the tag
    case "circle":
      return Math.PI * s.r * s.r;            // `s` is the circle here
    case "rect":
      return s.w * s.h;
  }                                          // every kind handled: no `return` needed after
}

const shapes: Shape[] = [{ kind: "circle", r: 1.0 }, { kind: "rect", w: 2.0, h: 3.0 }];
console.log(shapes.map((s) => area(s)));
```

The union has a Rust enum's layout: `kind` is the tag, not a stored string. A `switch` without
`default` must handle every kind. `switch` also works on numbers, strings, enums, `typeof x`
and unions of literals, with JavaScript's fallthrough and `break`
([Control flow](../reference/control-flow.md#switch)).

## Arrays, maps and closures

```ts
const xs = [1, 2, 3, 4];
let total = 0;
xs.forEach((x) => { total += x; });          // closures passed directly can update captures
const squares = xs.map((x) => x * x).filter((x) => x > 4);

const counts = new Map<string, i64>();        // insertion-ordered, like JS
counts.set("a", (counts.get("a") ?? 0) + 1);
for (const [word, n] of counts) console.log(word, n);

const [first, second] = [10, 20];
const point = { x: 1.0, y: 2.0 };
const merged = { ...point, z: 3.0 };
console.log(total, squares, first + second, merged);
```

## Memory in one minute

There is no garbage collector. Numbers and strings are values you can copy freely. Other
objects (arrays, class instances, maps) are references, as in JS, and are freed the moment
their last reference goes; calls borrow them, so passing an object to a function costs
nothing:

```ts
class User {
  name: string = "ann";
}

function log(u: User) { console.log(u.name); }
function save(u: User) { console.log("saved", u.name); }

const user = new User();
log(user);            // calls borrow: `user` stays usable
save(user);
```

There is no `mut`: the compiler infers which functions modify their arguments. Assigning an
object to a second variable refers to the same object, as in JS; `.clone()` makes a deep copy.
[Memory without a garbage collector](memory.md) explains the model.

## Async and I/O

```ts
import { readFile, writeFile } from "velt:fs";

async function fetchUser(id: i64): Promise<string> {
  await sleep(10);
  return `user ${id}`;
}

async function main() {
  await writeFile("hello.txt", "hi");
  console.log(await readFile("hello.txt"));

  const a = fetchUser(1);                            // starts now, like JS
  const b = fetchUser(2);                            // runs concurrently with a
  console.log(await a, await b);                     // ~10 ms in total

  const counter = shared(0);
  const tasks: Promise<void>[] = [];
  for (let i = 0; i < 100; i++) {
    tasks.push(spawn(async () => { counter.add(1); })); // on any core
  }
  await Promise.all(tasks);
  console.log(counter.get());                        // 100
}
```

Promises start when they are created, like in JS, and a directly awaited call costs nothing.
Forgetting an `await` is a compile error ("floating promise"). Spawned tasks run on every core;
they can't modify captured variables, so they share state through `shared(...)` or
`shared(new Mutex(...))` ([Async and concurrency](async.md)).

## An HTTP server

```ts
import { serve, Request, Response } from "velt:http";

async function main() {
  const hits = shared(0);
  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    hits.add(1);
    return Response.json({ path: req.path, hits: hits.get() });
  });
}   // like Node, a listening server keeps the program running (until `server.close()`)
```

`JSON.stringify` and `JSON.parse<T>` are generated at compile time for your types
([Building an HTTP server](http-server.md)).

## Modules

Modules work like TypeScript's ES modules, with named exports only:

```ts
import * as path from "velt:path";            // namespace import: path.join, path.basename, …
import { gcd as greatestDivisor } from "velt:math";

export function describe(file: string): string {
  return `${path.basename(file)} (${greatestDivisor(12, 18)})`;
}

console.log(describe("/tmp/a.txt"));
```

A folder is a module through its `index.vlt`, `import type` imports types only, and `paths`
in `package.vlt` replaces long `../../` chains ([Modules and packages](packages.md)).

## Where next

- [Velt for TypeScript developers](ts-developers.md): every difference, and why.
- The guides: [HTTP server](http-server.md), [command-line app](cli-app.md),
  [async](async.md), [errors](errors.md), [memory](memory.md), [testing](testing.md).
- The [standard library](../std/README.md).
