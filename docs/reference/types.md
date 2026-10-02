# Types

| Type | Meaning |
|---|---|
| `i8 i16 i32 i64 isize`, `u8 u16 u32 u64 usize` | fixed-width integers |
| `f32 f64`; `number` | floats; `number` is `f64` |
| `bool` | `true` / `false` |
| `string` | immutable UTF-8 text, a value ([Strings](#strings)) |
| `void`, `never` | no value; no possible value ([`switch`](control-flow.md#switch)) |
| `T[]` | growable array |
| `[A, B]` | tuple |
| `Map<K, V>` | insertion-ordered hash map |
| `{ a: T; b: U }` | anonymous object type |
| classes, structs, interfaces | [Classes](classes.md) |
| `A \| B`, `T \| null` | [unions](#union-types), [nullable values](#null) |
| `"up"`, `42`, `true` | [literal types](#literal-types) |
| `(x: T) => U` | function values: closures and named functions ([Functions](functions.md)) |
| `Promise<T>`, `shared<T>`, `Mutex<T>` | async results and thread-safe shared values ([Async](async.md)) |

Types are required on function parameters, and on return types other than `void` (a missing
return type means `void`). Everything else is inferred. `type Name = …` declares an alias; an
alias cannot refer to itself. There is no `any` or `unknown`: dynamic JSON is `JsonValue`
([`velt:json`](../std/json.md)).

## Numbers

Numbers behave like JavaScript numbers wherever the difference would show, while integer types
keep integer speed. Every integer value is either **declared** or **inferred**:

- **Declared**: its integer type is written or implied by a declaration: an annotated variable
  (`let n: i64 = 7`), a parameter, field or return type, a literal suffix (`7i32`), an `as`
  cast, an API result (`xs.length`, `s.indexOf(t)`, `Date.now()`), an array element or map
  value of an integer type, or an integer literal typed by such a context (`x + 2` with `x`
  declared, `f(2)`, `const n: u8 = 200`).
- **Inferred**: an integer literal with no context (`7`), a variable declared without a type
  whose initializer is inferred (`const a = 7`, `let i = 0`, `let n = a * 2`), and arithmetic
  with at least one inferred operand.

Both are stored as integers (inferred ones as `i64`), so loop counters, indexes and counts run
at integer speed. The rules:

- **`/` yields `f64` unless both operands are declared integers**: `const a = 7; a / 2` is
  `3.5`, `7 / 2` is `3.5`, `xs.length / 2` truncates (both declared), and
  `const h: i64 = 7 / 2` is `3`.
- **Integer division is explicit**: `Math.trunc(a / b)` with integer operands is one integer
  division instruction (truncating toward zero, exactly JS's `Math.trunc` of the quotient).
- Next to a float, or where a float is expected, an inferred integer converts: `a + 0.5`,
  `Math.sqrt(16)`, `const f: f64 = 1`. A declared integer never converts implicitly: write
  `x as f64`. Different declared integer types don't mix either: `xs.length` is a `usize`, so
  compare it with a `usize` (`let i: usize = 0`) or cast (`i as usize`).
- `x /= y` on an integer variable is allowed only when it is integer division; otherwise it is
  an error (it would store a float).
- `%` on integers is the remainder truncated toward zero (sign of the dividend), like JS.
- A float operand of a bitwise operator converts like JS's ToInt32 (`(a / 13) | 0` truncates;
  `NaN` and ±Infinity give 0); the result is an inferred integer.
- `as` converts between number types with Rust semantics: floats truncate and saturate
  (`3.9 as i64` is `3`), integers wrap (`300 as u8` is `44`, `-1 as u8` is `255`).
- Differences from JS that remain: integers wrap at their width instead of losing precision
  past 2^53; integer `/ 0` and `% 0` panic (float division gives `Infinity`/`NaN` as in JS);
  `**` on integers is integer power.
- Floats print like JS: `10`, `1.5`, `0.30000000000000004`, `1e+21`, `NaN`, `Infinity`; `-0`
  prints `0`.

```ts
const a = 7;                              // inferred: behaves like a JS number
console.log(a / 2, a + 0.5);              // 3.5 7.5
const n: i64 = 7;                         // declared: integer arithmetic
console.log(n / 2, Math.trunc(a / 2));    // 3 3
let small: u8 = 250;
small += 10;                              // wraps: 4
console.log(small, n as f64 / 2.0, 300 as u8);   // 4 3.5 44
```

## Strings

A `string` is an immutable value, like in JS: assign it, pass it, return it, store it, capture
it, take it out of a field, an array element, a `for...of` element or `Map.get`. The source stays
usable and no copy method is needed.

- `+` concatenates two strings; `s += x` appends (in place when `s` holds the only reference to
  its text; other copies of `s` never change).
- **No implicit conversion**: `"Total: " + 5` and `"a" + true` are compile errors. Build text
  with a template literal (`` `Total: ${n}` ``), which formats any value the way `console.log`
  does.
- `s.length` is the **byte** length (`usize`); positions (`slice`, `indexOf`, regex offsets) are
  byte offsets. There is no `s[i]` indexing and no `for...of` over a string; use `slice` or
  `charCodeAt`.
- Methods: `slice substring indexOf lastIndexOf includes startsWith endsWith split trim
  trimStart trimEnd toUpperCase toLowerCase replace replaceAll repeat padStart padEnd
  charCodeAt`, plus `String.fromCharCode`, `parseInt`, `parseFloat` and `Number(s)`
  ([prelude](../std/prelude.md#strings)).
- `<` and `>` compare bytewise; `==` compares content.
- Cost model: strings of up to 23 bytes are stored inline (no heap allocation); longer ones live
  in a reference-counted immutable buffer. A copy is 24 bytes plus, for a heap string, one count
  increment, and the compiler moves instead of copying at a last use. `s.clone()` compiles and
  is just a copy.

```ts
function label(name: string, count: i64): string {
  let s = name;                   // a copy: `name` stays usable
  s += ":";
  return `${s} ${count} (${name.length} bytes)`;
}

console.log(label("tea", 3), "a,b".split(","), "  x ".trim().padStart(3, "*"));
```

## Equality and comparison

- `==` and `===` are the same operator, as are `!=` and `!==`: there is no coercion, and both
  operands must have the same type (`1 == "1"` is a compile error; an `i64` compared with an
  `i32` needs a cast).
- Numbers, bools and strings compare by value. Objects (class instances, arrays, maps,
  structs, object literals, interface and function values) compare by **identity**, like JS:
  `[1] == [1]` is `false`, and `a == b` is `true` when `b` refers to the same object as `a`.
  `T | null`, unions and tuples compare their parts that way.
- Content comparison: `deepEqual(a, b)` ([prelude](../std/prelude.md)) compares arrays,
  structs and object literals by their contents, recursively, and class instances (`Map`
  included) by identity; `assertEq` uses it.
- `<`, `<=`, `>`, `>=` work on numbers and strings, and on a generic `T extends Comparable<T>`
  ([Comparable](classes.md#comparable)).

## Null

`null` is the only "nothing". `undefined` is not part of the language: `undefined` as a value or
a type, and `void expr`, are errors with the fix "use `null`" (editors offer it as a quick fix),
and `typeof x === "undefined"` is an error too (test `x === null`). A value that may be absent
has type `T | null`, stored without an extra allocation where possible.

- `x ?? d` (default), `x?.f` / `x?.m()` (optional access; the result is nullable),
  `if (x != null) { … }` and early exits narrow `x` to `T` (a local or a field path of one,
  see below); `switch` supports `case null`.
- `a?: T` is `T | null` everywhere: an optional parameter `b?: T` is `b: T | null = null`
  (callers may leave it out or pass `null`; it cannot also have a default), an optional class
  or interface field starts as `null` (and is omitted by `JSON.stringify` when null), and an
  object literal may leave out any `T | null` field of an object type
  (`{ port: i64; host?: string }` accepts `{ port: 80 }`).
- `JSON.parse<T>` treats an absent key like an explicit `null` (a `T | null` field may be
  missing); only a `JsonValue` tells them apart: `v.has("a")` vs `v.get("a")?.isNull()`.
- `x?.a.b` short-circuits the rest of the chain like TypeScript (null when `x` is null; `.b` is
  never evaluated). Parentheses end a chain: `(x?.a).b` needs `x?.a` to be non-null.
- Narrowing applies to locals and to field paths of locals (`this.x`, `node.left`), like
  TypeScript; assigning a non-null value narrows too. A narrowed field is re-checked when read,
  so a call that set it to `null` in between panics instead of reading `null`. Inside a
  closure, a variable narrowed where the closure is created stays narrowed (the closure may not
  assign it).
- `const x = node.left` / `const row = grid[i]` refers to the same object as the field or
  element (objects are references, [Memory model](memory.md#values-and-references)); when the
  rest of the block replaces `node.left`, `x` keeps referring to the old object, as in JS.

```ts
class User {
  name: string = "ann";
  nickname?: string;              // starts as null
}

function greet(name: string, greeting?: string): string {
  return `${greeting ?? "Hello"}, ${name}`;   // greet("ann") works: greeting is null
}

function first(xs: i64[]): i64 | null {
  return xs.length > 0 ? xs[0] : null;
}

function doubledFirst(xs: i64[]): i64 | null {
  const f = first(xs);
  if (f == null) {
    return null;                  // early exit: f is i64 below
  }
  return f * 2;
}

const u: User | null = new User();
console.log(u?.name, u?.nickname ?? "(none)", doubledFirst([]) ?? -1, greet("bo"));
const f = first([4, 5]);
if (f != null) {
  console.log(f + 1);             // f is i64 here
}
```

## Literal types

A string, number or bool literal is a type with that one value: `"circle"`, `42`, `-1`, `1.5`,
`5u8`, `true`. `type Dir = "up" | "down"` is a union of literal types.

- A literal takes a literal type only where one is expected (an annotation, a parameter, a
  field, a union with literal members); elsewhere it has its base type (`const s = "up"` is a
  `string`). A literal-typed value converts implicitly to its base type (`const s: string = d;`).
- A literal type is zero-sized; a union of literals is only its tag. Printing, `${}` and
  `JSON.stringify` show the value; `typeof` gives the base type's tag.
- `JSON.parse` into literal types is not supported yet.

## Union types

`A | B | C` of any types is a tagged union. Member order and nesting don't matter; `T | null` is
the nullable type; `void` cannot be a member.

- **Widening** is implicit: a member value (also a subclass of a class member, or a value
  implementing an interface member) converts to the union, and a union converts to a wider
  union. A literal takes the member it belongs to; add a suffix (`5i32`) when several number
  members match.
- **Narrowing**: using a member's fields or methods needs that member.
  - `typeof x === "string" | "number" | "boolean" | "object" | "function"` (and `!==`): all
    number types are `"number"`; classes, structs, arrays, maps and `null` are `"object"`;
    closures are `"function"`. An impossible tag is an error.
  - `x instanceof C` matches members whose class is `C` or a subclass. A downcast (testing a
    base-class value for a subclass) is an error: use a union of the subclasses.
  - `x == literal` / `x != literal` selects the literal's member.
  - Conditions of `if`, `while`, `&&`, `||`, `!`, ternaries and early exits narrow a local
    until it is reassigned; `switch` narrows each case ([`switch`](control-flow.md#switch)).
- Printing and template literals show the active member's value. `JSON.stringify` works on
  unions; `JSON.parse` cannot decode them yet.
- A union of numbers, bools, strings and literals is copied; one holding an object refers to
  it like any other variable.

```ts
class NotFound {
  id: string;
  constructor(id: string) {
    this.id = id;
  }
}

function find(id: string): string | NotFound {
  return id == "1" ? "ann" : new NotFound(id);
}

function show(v: string | i64): string {
  if (typeof v === "string") {
    return v.toUpperCase();
  }
  return `${v + 1}`;
}

const r = find("2");
if (r instanceof NotFound) {
  console.log("missing", r.id);
} else {
  console.log(r);
}
console.log(show("a"), show(41));
```

## Discriminated unions

A union of object types (anonymous object types, structs or classes) whose members all have a
field of a literal type is a **discriminated union**; that field (any name; `kind` below) is the
*discriminant*. It is compiled like a Rust enum: the discriminant is the tag, not a stored
string.

```ts
type Shape =
  | { kind: "circle"; r: f64 }
  | { kind: "rect"; w: f64; h: f64 };

function area(s: Shape): f64 {
  switch (s.kind) {
    case "circle":
      return Math.PI * s.r * s.r;   // s is the circle member here
    case "rect":
      return s.w * s.h;
  }                                 // every kind handled: no return needed after the switch
}

function isRound(s: Shape): bool {
  return s.kind === "circle";
}

const shapes: Shape[] = [{ kind: "circle", r: 1.0 }, { kind: "rect", w: 2.0, h: 3.0 }];
for (const s of shapes) {
  console.log(s.kind, area(s), isRound(s));
}
```

- An object literal where the union is expected picks its member by the discriminant's literal
  (else by its field names); an impossible discriminant is an error
  (``"square"` is not a valid `kind` for `Shape` ``).
- `x.kind === "circle"` / `!==` and `switch (x.kind)` narrow a local `x`; comparing with an
  impossible literal is an error.
- A field every member has (like `kind`) can be read without narrowing; other fields need
  narrowing (``no field `r` on type `Shape` ``). Fields cannot be assigned through the union.
- Recursive discriminated unions need a nominal member (a class or struct:
  `class Node { kind: "node"; kids: Tree[] }`), because an alias cannot refer to itself.
- Payload enums and `match` do not exist; both are errors with a hint to use a discriminated
  union.

## Enums

TypeScript-style enums only:

- numeric `enum Color { Red, Green = 5, Blue }`: auto-incremented; `Color.Blue as i64` is its
  number; printed as a number;
- string `enum Dir { Up = "UP", Down = "DOWN" }`: every member needs a string; printed and
  serialized as its string; converts to `string`; no `as` cast.

Enums are not generic and have no payloads; use a discriminated union for tagged data.

## Objects, arrays, tuples and maps

- **Object literals** `{ name: "a", n: 1 }` have anonymous object types
  `{ name: string; n: i64 }` with a fixed layout (a field access is one load). An object type
  accepts exactly its fields: extra fields are a type error, and adding a property later is an
  error (use a `Map`).
- **Spread**: `{ ...a, b: 1 }` builds a merged object at compile time (later keys win);
  `[x, ...xs]` builds a new array. Spread arguments, `f(...xs)`, are not supported.
- **Destructuring**: `const [a, b] = pair;`, `const [head, ...rest] = xs;`,
  `const { a, b } = obj;`, and `for (const [k, v] of map)`. Defaults inside patterns and
  parameter patterns are not supported. Array destructuring checks the length like indexing: a
  shorter array panics with the same `index out of bounds` message.
- **Arrays** `T[]`: `length` (`usize`), `xs[i]` (bounds-checked: panics
  `index out of bounds: the len is L but the index is I`), `push`, `pop(): T | null`,
  `forEach map filter reduce find findIndex some every indexOf lastIndexOf includes slice concat
  reverse isEmpty entries fill`, `join` (any elements, shown as `${x}` shows them), `sort()` on
  numbers, strings and `Comparable` elements, and `sort(cmp)` (stable, any element type, like
  JS's `Array.prototype.sort(compareFn)`). The full list is in the
  [prelude](../std/prelude.md#arrays).
- `new Array<T>(n).fill(v)` and `Array.from({ length: n }, (_, i) => f(i))` build an array of
  `n` elements in one allocation. A bare `new Array<T>(n)` is an error: arrays have no holes.
- **Tuples** `[A, B]`: `t[0]`, destructuring, printed like arrays. `Promise.all` over tuples of
  different types is not supported.
- **`Map<K, V>`**: `new Map<K, V>()`, `set`, `get(k): V | null` (the stored value itself, as in
  JS), `has`, `delete`, `size`, `keys()`, `values()`, `entries()`, `for (const [k, v] of m)`,
  plus single-lookup updates: `upsert(k, init, (v) => v + 1)`,
  `update(k, (v) => { v.push(x); }): bool` (the callback gets the stored value itself) and
  `getOrInsert(k, () => v)`. Keys: numbers, `bool`, `string`, class instances (by identity),
  and structs, object types and tuples, which compare by content (in JS two equal object
  literals are two different keys). Iteration follows insertion order, like JS.
- `JSON.stringify(x)` / `JSON.parse<T>(s)` are generated at compile time for numbers, bools,
  strings, arrays, enums, nullable values, structs, classes and anonymous objects
  ([`velt:json`](../std/json.md)).

```ts
struct Point {
  x: i64;
  y: i64;
}

const p: Point = { x: 1, y: 2 };
const moved = { ...p, y: 5, label: "moved" };
const [first, ...rest] = [10, 20, 30];
const counts = new Map<string, i64>();
for (const word of "a b a".split(" ")) {
  counts.upsert(word, 1, (n) => n + 1);
}
for (const [word, n] of counts) {
  console.log(word, n);
}
console.log(moved, first, rest, [3, 1, 2].map((x) => x * 2).filter((x) => x > 2));
```
