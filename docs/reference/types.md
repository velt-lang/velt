# Types

| Type | Meaning |
|---|---|
| `i8 i16 i32 i64 isize`, `u8 u16 u32 u64 usize` | fixed-width integers |
| `f32 f64`; `number` | floats; `number` is `f64` |
| `boolean`, `bool` | `true` / `false`; one type with two names ([Booleans](#booleans)) |
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
alias cannot refer to itself, and it is checked even where nothing uses it. There is no `any`
or `unknown`: dynamic JSON is `JsonValue` ([`velt:json`](../std/json.md)).

## Booleans

`boolean` and `bool` are the same type, and either name can be used anywhere a type is written:
annotations, generic arguments, unions and function types. `boolean` is TypeScript's name, so
code that is shared with TypeScript uses it; `bool` is the shorter name Velt code has used.
Diagnostics, hover and inlay hints print `boolean`; `velt fmt` keeps the name you wrote.

```ts
function isEven(n: i64): boolean {
  return n % 2 == 0;
}

const check: (n: i64) => bool = isEven;
const flags: (boolean | null)[] = [check(2), null];
console.log(flags);               // [ true, null ]
```

## Numbers

Numbers behave like JavaScript numbers wherever the difference would show, while integer types
keep integer speed. Every integer value is either **declared** or **inferred**:

- **Declared**: its integer type is written or implied by a declaration: an annotated variable
  (`let n: i64 = 7`), a parameter, field or return type, a literal suffix (`7i32`), an `as`
  cast, an array element or map value of an integer type, or an integer literal typed by such a
  context (`x + 2` with `x` declared, `f(2)`, `const n: u8 = 200`).
- **Inferred**: an integer literal with no context (`7`), a variable declared without a type
  whose initializer is inferred (`const a = 7`, `let i = 0`, `let n = a * 2`), a field declared
  without a type from an integer literal (`count = 0;`), arithmetic with at least one inferred
  operand, and every integer the standard library hands to your code: `xs.length`,
  `s.indexOf(t)`, `m.size`, `Date.now()`, the index of `entries()` and of array callbacks. You
  wrote no integer type for those, so they are JS numbers (inside `std/` they stay declared).

Both are stored as integers (inferred ones as `i64`), so loop counters, indexes and counts run
at integer speed; a local declared from a length (`let n = xs.length`) is an `i64` like any
other inferred one, so `n -= 5` can go below zero. The rules:

- **`/` yields `f64` unless both operands are declared integers**: `const a = 7; a / 2` is
  `3.5`, `7 / 2` is `3.5`, `xs.length / 2` is `1.5` for three elements, and
  `const h: i64 = 7 / 2` is `3`.
- **Integer division is explicit**: `Math.trunc(a / b)` with integer operands is one integer
  division instruction (truncating toward zero, exactly JS's `Math.trunc` of the quotient).
- Next to a float, or where a float is expected, an inferred integer converts: `a + 0.5`,
  `Math.sqrt(16)`, `const f: f64 = 1`. Next to an integer of another type, or where one is
  expected, it adapts: `let i = 0; i < xs.length` and `s.slice(0, s.length - 1)` compile as in
  JS. A declared type other than `usize` wins; otherwise both sides become `i64`, so
  `let i = -1; i < xs.length` is `true`. Compound assignments adapt the same way, converting the
  value to the target's type: `let total = 0; total += s.length`. A declared integer never
  converts implicitly: write `x as f64`, and different declared integer types don't mix
  (`let n: i32 = 1; let m: u8 = 2; n < m` is an error).
- **A float index** (`xs[i]` with `i: number`, `xs[Math.floor(n / 2)]`, `xs[parseInt(s)]`) must
  be a whole number at run time; anything else panics like an index out of bounds (JS reads
  `undefined`). Indexing with a quotient directly, `xs[n / 2]`, stays an error: write
  `Math.trunc(n / 2)`.
- A float passed to an integer parameter of a standard library function converts like JS's
  `ToIntegerOrInfinity`: `xs.slice(0, xs.length / 2)` takes the first half.
- `x /= y` on an integer variable is allowed only when it is integer division; otherwise it is
  an error (it would store a float).
- `%` on integers is the remainder truncated toward zero (sign of the dividend), like JS.
- **Bitwise operators on numbers are JS's 32-bit operators.** When no operand is a declared
  integer, `| & ^ << >> ~` take ToInt32 of their operands (truncate, then wrap modulo 2^32 into
  the signed 32-bit range; `NaN` and ±Infinity give 0) and `>>>` takes ToUint32; shift counts
  are taken modulo 32, and the result is an inferred integer: `(a / 13) | 0` truncates,
  `-1 >>> 0` is `4294967295`, `1 << 32` is `1`. A product inside such an operand rounds like
  JS's double multiply once it is past 2^53, so `(y * 0x2c1b3c6d) | 0` is Node's value;
  `Math.imul(y, 0x2c1b3c6d)` is the 32-bit wrapping product (one instruction). They compile to
  32-bit integer instructions. Operands of a declared integer type keep their own width
  (`n >>> 3` with `n: i64` is a 64-bit shift), and so does a constant of two literals where an
  integer type is expected (`const m: u64 = 1 << 40`).
- `as` converts between number types with Rust semantics: floats truncate and saturate
  (`3.9 as i64` is `3`), integers wrap (`300 as u8` is `44`, `-1 as u8` is `255`).
- Differences from JS that remain: integers wrap at their width instead of losing precision
  past 2^53 (outside bitwise operands, an inferred product like `m * m` stays exact); integer `/ 0` and `% 0` panic (float division gives `Infinity`/`NaN` as in JS);
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
const h = 0x12345678;
console.log((h * 0x2c1b3c6d) | 0, Math.imul(h, 0x2c1b3c6d), -1 >>> 0); // -1019940576 -1019940584 4294967295
```

## Strings

A `string` is an immutable value, like in JS: assign it, pass it, return it, store it, capture
it, take it out of a field, an array element, a `for...of` element or `Map.get`. The source stays
usable and no copy method is needed.

- `+` concatenates two strings; `s += x` appends to a variable or field in place when `s` holds
  the only reference to its text, growing it geometrically, so building a string in a loop costs
  time linear in its length. `s = s + x` and `` s = `${s}${x}` `` append the same way. Other
  copies of `s` never change.
- **No implicit conversion**: `"Total: " + 5` and `"a" + true` are compile errors. Build text
  with a template literal (`` `Total: ${n}` ``), which formats any value the way `console.log`
  does.
- `s.length` is the **byte** length; positions (`slice`, `indexOf`, regex offsets, `s[i]`) are
  byte offsets. For ASCII text that is JS's answer; for other text it differs
  (`"Zoë".length` is 4, where JS says 3).
- `s[i]` is `s.charAt(i)`: the character starting at position `i`, or `""` past the end (JS:
  `undefined`). `for (const c of s)` iterates the characters (`s.split("")`), emoji included.
- Methods: `slice substring indexOf lastIndexOf includes startsWith endsWith split trim
  trimStart trimEnd toUpperCase toLowerCase replace replaceAll repeat padStart padEnd charAt at
  charCodeAt`, plus `String.fromCharCode`, `parseInt`, `parseFloat` and `Number(s)`
  ([prelude](../std/prelude.md#strings)).
- `<` and `>` compare bytewise; `==` compares content.
- Cost model: strings of up to 23 bytes (22 when they are not ASCII) are stored inline (no heap
  allocation); longer ones live in a reference-counted immutable buffer. A copy is 24 bytes
  plus, for a heap string, one count increment, and the compiler moves instead of copying at a
  last use. `s.clone()` compiles and is just a copy.
- A string holds less than 2 GiB of text (more than JS engines allow). Making a longer one stops
  the program with `string too long` (`repeat` panics with JS's `RangeError` message instead).

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
  `T | null`, unions and tuples compare their parts that way. A `T | null` compares with a
  `T` (in either order) as if both were `T | null`: `null` equals no value.
- Content comparison: `deepEqual(a, b)` ([prelude](../std/prelude.md)) compares arrays,
  structs and object literals by their contents, recursively; maps and records by their keys
  and values, in any key order; other class instances by identity. `assertEq` uses it.
- `<`, `<=`, `>`, `>=` work on numbers and strings, and on a class or struct implementing
  `Comparable` (`Date`, `DateTime`) or a generic `T extends Comparable<T>`: `a < b` is
  `a.compareTo(b) < 0` ([Comparable](classes.md#comparable)).

## Null

`null` is the only "nothing". `undefined` is not part of the language: `undefined` as a value or
a type, and `void expr`, are errors with the fix "use `null`" (editors offer it as a quick fix),
and `typeof x === "undefined"` is an error too (test `x === null`). A value that may be absent
has type `T | null`, stored without an extra allocation where possible.

- `x ?? d` (default), `x?.f` / `x?.m()` (optional access; the result is nullable),
  `if (x != null) { … }` and early exits narrow `x` to `T` (a local or a field path of one,
  see below); `switch` supports `case null`.
- `x ??= d` assigns `d` when `x` is `null` and narrows `x` (likewise `x ||= d` and `x &&= d`).
  The target may not call a function yet (`m[key()] ??= v`): store the key in a variable first.
- `x!` is `x` known not to be `null` (TS's non-null assertion). TypeScript trusts it; Velt
  checks it: a `null` panics with `non-null assertion failed`.
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
- Literal types work as type arguments too, negative numbers included: `f<"x" | null>()`,
  `g<-1>(5)`. As in TypeScript, `a < -1` stays a comparison: a literal after `<` starts type
  arguments only when `>`, `|` or `,` follows it.
- A literal type is zero-sized; a union of literals is only its tag. Printing, `${}` and
  `JSON.stringify` show the value; `typeof` gives the base type's tag.
- `JSON.parse` checks a literal type against its value (`expected "task" at $.kind`).
- `e as const` is accepted and keeps the value as it is: Velt arrays and literals need no
  `readonly` or literal-type annotation for it.

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
  unions; `JSON.parse` decodes them when the JSON value tells the members apart (discriminated
  unions by their discriminant; see [`velt:json`](../std/json.md)).
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
  impossible literal is an error. A `bool` discriminant is also a condition:
  `if (r.done)` / `if (!r.done)` narrow `r` of `{ value: T; done: false } | { done: true }`.
- A field every member has (like `kind`) can be read without narrowing; other fields need
  narrowing (``no field `r` on type `Shape` ``), except `value` on an `IteratorResult<T>`, which
  reads as `T | null` ([Iterables](control-flow.md#iterables)). Fields cannot be assigned through
  the union.
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
  error (use a `Map` or a `Record`).
- **`readonly` fields**: in `{ readonly id: i64; name: string }`, assigning `id` is an error
  (``cannot assign to `id`: it is a readonly field``); like TypeScript's, the check is shallow
  (`u.tags.push(x)` is fine). A value converts between a type and the same type without
  `readonly`, in both directions, and stays the same object.
- **Utility types** build an object type from a concrete one (an object type, an interface
  with only fields, or a class or struct, whose public fields are used):
  `Partial<T>` (every field optional), `Required<T>` (every nullable field non-null),
  `Readonly<T>` (every field `readonly`), `Pick<T, K>` (only the fields named in `K`) and
  `Omit<T, K>` (every other field). `K` is a string literal type or a union of them
  (`"id" | "email"`). In `Pick` a name that is not a field is an error; in `Omit` it is a
  warning, as TypeScript accepts it (so `type WithoutChildren<P> = Omit<P, "children">` works
  on types without `children`). The results are ordinary object types: `Pick<User, "name">`
  *is* `{ name: string }`, and declaration order doesn't matter. Differences from TypeScript:
  `Required` also removes `null` from fields written `a: T | null` (in Velt `a?: T` is
  `T | null`, #418); an operator on a type parameter (`Partial<T>` in a generic function) is
  not supported yet (#350); and a type can't apply one to itself in its own fields
  (`interface Node { patches: Partial<Node>[] }`).

```ts
interface User {
  readonly id: i64;
  name: string;
  email?: string;
}

type Patch = Partial<User>;               // { readonly id?: i64; name?: string; email?: string }
type Summary = Pick<User, "id" | "name">; // { readonly id: i64; name: string }

function apply(u: User, p: Patch): User {
  return { id: u.id, name: p.name ?? u.name, email: p.email ?? u.email };
}

const s: Summary = { id: 1, name: "ann" };
console.log(apply({ id: s.id, name: s.name }, { email: "a@x" }).email); // a@x
```

- **Spread**: `{ ...a, b: 1 }` builds a merged object at compile time (later keys win);
  `[x, ...xs]` builds a new array (integer elements spread into a `number[]` convert). Spread
  arguments, `f(...xs)`, fill a rest parameter ([Functions](functions.md)). Whatever `for...of`
  takes can be spread into an array or a rest parameter too: `[..."héllo"]` (characters),
  `[...map]` (entries), `[...gen()]`, `Math.max(...set)`
  ([Consuming an iterable](control-flow.md#consuming-an-iterable)).
- **Destructuring**: `const [a, b] = pair;`, `const [head, ...rest] = xs;`,
  `const { a, b } = obj;`, and `for (const [k, v] of map)`. Array destructuring checks the
  length like indexing: a shorter array panics with the same `index out of bounds` message.
  A string, a map or an iterable is destructured like in JS (`const [first, ...rest] = "abc"`):
  `const [a, b] = gen()` takes two values and closes the iterator; one that has fewer values
  panics like a short array, unless the pattern gives defaults. Nested patterns work too
  (`const [[a, b], [c]] = [gen(), gen()]`).
- **Defaults** in `const` and `let` patterns: `const { host = "localhost", port = 80 } = opts;`
  takes the default when the field is `null`, and `const [first = 0] = xs;` when the array is
  too short (where JS reads `undefined`). Defaults in `for...of` patterns and parameter patterns
  are not supported.
- **Arrays** `T[]`: `length`, `xs[i]` (bounds-checked: panics
  `index out of bounds: the len is L but the index is I`), `push`, `pop(): T | null`,
  `forEach map filter reduce find findIndex some every indexOf lastIndexOf includes slice concat
  reverse isEmpty entries fill`, `join` (any elements, shown as `${x}` shows them), `sort()` on
  numbers, strings and `Comparable` elements, and `sort(cmp)` (stable, any element type, like
  JS's `Array.prototype.sort(compareFn)`). Callbacks get the element and its index, like JS
  (`xs.map((x, i) => …)`), and may take fewer parameters. The full list is in the
  [prelude](../std/prelude.md#arrays). Arrays, strings and maps are
  [`Iterable`](control-flow.md#iterables): they convert to `Iterable<T>` values, and
  `xs[Symbol.iterator]()` returns an `Iterator<T>`.
- `new Array<T>(n).fill(v)` and `Array.from({ length: n }, (_, i) => f(i))` build an array of
  `n` elements in one allocation. A bare `new Array<T>(n)` is an error: arrays have no holes.
  `Array.from(src)` and `Array.from(src, (v, i) => …)` copy (and map) anything `for...of` takes:
  an array, a string's characters, a map's entries, a generator, any iterable.
- **Tuples** `[A, B]`: `t[0]`, destructuring, printed like arrays. `Promise.all` over tuples of
  different types is not supported.
- **`Map<K, V>`**: `new Map<K, V>()`, `new Map(entries)` from an array of `[key, value]` tuples
  (`new Map([["a", 1], ["b", 2]])`: as in JS, the array stays as it is, the map shares its keys
  and values, and a repeated key keeps its first position and its last value) or from any
  iterable of them (`new Map(pairs())`), `set`,
  `get(k): V | null` (the stored value itself, as in JS), `has`, `delete`, `size`, `keys()`,
  `values()`, `entries()`, `for (const [k, v] of m)`, plus single-lookup updates: `upsert(k, init, (v) => v + 1)`,
  `update(k, (v) => { v.push(x); }): bool` (the callback gets the stored value itself) and
  `getOrInsert(k, () => v)`. Keys: numbers, `bool`, `string`, class instances (by identity),
  and structs, object types, tuples, arrays, maps and records, which compare by content (in JS
  two equal object literals are two different keys). Float keys compare like JS's
  (SameValueZero: `0` and `-0` are one key, `NaN` finds itself), and a content key changed
  after insertion makes its entry unreachable
  ([Map](../std/prelude.md#map)). Iteration follows insertion order, like JS.
- **`Record<K, V>`**: a dictionary written with object syntax, like TypeScript's `Record`.
  `K` is `string`, a union of string literal types, or a string enum; any other key type is
  an error (use a `Map`), also when a generic function or class gets it as a type argument.
  With `string` keys a record is *open*: `r[k]` and `r.name` are `V | null`, `r[k] = v`
  inserts or replaces, `r[k] ??= v` sets a missing key, and `delete r[k]` removes. Because a
  key may be missing, `r[k] += 1`, `r[k]++` and the other compound assignments are errors on an
  open record: say what a missing key starts from with `r[k] = (r[k] ?? 0) + 1` (JS would give
  `NaN`). With literal or enum keys it is *closed*: it always holds every key, so `r.cpu` is
  `V`, `r.cpu += 1` works, a typo is an error, and `delete` is not allowed.
  On an enum-keyed record, `r.mem` names the member whose value is `"mem"`. Build a record
  from an object literal where a record is expected (`const r: Record<string, i64> = {}`; a
  closed record's literal must list every key) or with `new Record<string, V>()`. A literal
  may spread another record (`{ ...r, x: 1 }`). In generic code, where the key type is a type
  parameter `K`, reads are `V | null` and the record may be closed, so it cannot start empty
  (only a literal with a spread builds one) and `delete` is not allowed. A record has no
  methods of its own and is not iterable: `Object.keys(r)` (a `string[]`), `Object.values(r)`
  and `Object.entries(r)` return arrays in insertion order (`for (const [k, v] of
  Object.entries(r))`). Given an object literal, `Object.values` and `Object.entries` read it
  as a `Record<string, V>`, so its values need one type. `Object.keys` accepts any object, as
  in TypeScript: an object literal or object type (`Object.keys({ a: 1, b: "x" })` is `["a",
  "b"]`), a struct, or a class instance, whose fields it lists in declaration order (base class
  fields first, `private` ones too; not static fields or methods). A struct's optional field is
  listed only when it is not `null`. A class with subclasses is an error, because the value may
  be a subclass instance with more fields. `console.log` and `JSON` treat a record as an object. A class
  cannot `extends` a `Record` (its constructor would leave a closed record without its keys);
  hold one in a field instead. A literal for an enum-keyed record is not supported yet.
- `JSON.stringify(x)` / `JSON.parse<T>(s)` are generated at compile time for numbers, bools,
  strings, literal types, arrays, tuples, enums, nullable values, `Map<string, V>`,
  `Record<K, V>`, structs, classes and anonymous objects ([`velt:json`](../std/json.md)). A
  struct or class with a `private` field has no JSON form (a compile error naming the field):
  private fields stay private, and runtime handles can't be forged from JSON.

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
const ports = new Map([["http", 80], ["https", 443]]); // Map<string, i64>
console.log(ports.get("https")); // 443
```

### Iterable object literals

An object literal whose one member is a `[Symbol.iterator]()` method is an `Iterable<T>`:
`for...of`, spread, destructuring, `Array.from` and `Iterable<T>` parameters take it, and each
of them calls the method again.

```ts
function range(from: i64, to: i64): Iterable<i64> {
  return {
    *[Symbol.iterator](): Generator<i64> {
      for (let v = from; v <= to; v++) {
        yield v;
      }
    },
  };
}

console.log([...range(1, 3)]); // [ 1, 2, 3 ]
```

- The method is a generator (`*[Symbol.iterator](): Generator<T>`), or returns an iterator
  (`[Symbol.iterator](): Iterator<T> { return new Countdown(3); }`); `async
  *[Symbol.asyncIterator](): AsyncGenerator<T>` makes an `AsyncIterable<T>` for
  [`for await`](control-flow.md#for-await). The method uses the variables around it, as a
  [generator function expression](functions.md#generator-function-expressions) does.
- Object literals are plain data in Velt, so this is their only method. The literal can have no
  other members, and `this` in the method is an error (in TS it is the object): use variables,
  or declare a class that `implements Iterable<T>` and reads its fields. Other methods in object
  literals are errors too: write a property holding an arrow function.
- The value is an instance of the prelude class `__IterableObject<T, E>` (async:
  `__AsyncIterableObject<T, E>`), which holds the method; annotate it as `Iterable<T>`.
