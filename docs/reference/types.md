# Types

| Type | Meaning |
|---|---|
| `i8 i16 i32 i64 isize`, `u8 u16 u32 u64 usize` | fixed-width integers |
| `f32 f64`; `number` | floats; `number` is `f64`, a JavaScript number ([Numbers](#numbers)) |
| `boolean`, `bool` | `true` / `false`; one type with two names ([Booleans](#booleans)) |
| `string` | immutable text, indexed in UTF-16 code units (stored as UTF-8), a value ([Strings](#strings)) |
| `void`, `never` | no value; no possible value ([`switch`](control-flow.md#switch)) |
| `T[]` | growable array |
| `[A, B]` | tuple |
| `Map<K, V>` | insertion-ordered hash map |
| `{ a: T; b: U }` | anonymous object type |
| classes, structs, interfaces | [Classes](classes.md) |
| `A \| B`, `T \| null` | [unions](#union-types), [nullable values](#null) |
| `A & B`, `T["k"]` | [intersections of object types](#intersection-types), [branded types](#branded-types), [indexed access](#indexed-access-types) |
| `"up"`, `42`, `true` | [literal types](#literal-types) |
| `(x: T) => U` | function values: closures and named functions ([Functions](functions.md)) |
| `Promise<T>`, `shared<T>`, `Mutex<T>` | async results and thread-safe shared values ([Async](async.md)) |

Types are required on function parameters. A missing return type is inferred from the
function's `return`s ([Return types](functions.md#return-types)), and everything else is
inferred too. `type Name = …` declares an alias; it is checked even where nothing uses it. An
alias may refer to itself only when it is an object type, or an intersection of object types,
written out (`type Tree = { kids: Tree[]; v: number }`): it is then the interface with those
fields ([Intersection types](#intersection-types)). There is no `any`
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

`number` is a JavaScript number, always: an IEEE-754 double, with `-0`, `NaN`, `±Infinity` and
rounding past 2^53, exactly as in Node. The compiler stores a `number` as a 32- or 64-bit
integer where it proves the result is identical (loop counters, indexes, `% m` sums, the result
of `| 0`), so integer code runs at integer speed without changing what it computes.
`velt build --report numbers` lists the `number` variables in loops that stay doubles, and why.

Integer types (`i8`..`i64`, `u8`..`u64`, `isize`, `usize`) and `f32` are Velt's opt-in for speed
and memory where you want a fixed width; code shared with TypeScript never names them.

- **Literals**: `7` and `0.5` are numbers. Where an integer type is expected, a literal takes it:
  `const n: i64 = 7`, `n + 1` with `n: i64`, `f(2)` with an `i32` parameter, `300 as u8` (`44`).
  A suffix states the type: `7u8` is a `u8`. A module-level `const N = 4` stands for its literal
  wherever it is used (TS types it `4`): `i < N` with `i: i64` compares integers. `shared(0)` is
  a `shared<i64>`, an atomic counter.
- **Locals declared from literals**: `let i = 0` declares a number, unless the function uses `i`
  only with one integer type `T` and never as a number; then `i` is a `T`, as if declared
  `let i: T = 0`. So `let steps = 0; … return steps` in a function returning `i64` counts in an
  `i64`, and `let i = 0; i < n` with `n: i64` makes `i` an `i64`. A use with `T` is assigning,
  passing, returning or storing the local (or arithmetic on it: `acc + i`) where a `T` is
  expected, or combining or comparing it with a 64-bit integer of type `T`. A use as a number is
  combining it with another number (`0.5`, `xs.length`), `/`, `**`, a `number` parameter or a
  method call on it, and a bitwise operator unless the other operand is a 64-bit integer
  (`x << 40` shifts by 8, as in JS). Arithmetic (`+ - * %`, `-x`, `+=`, `++`) on a local
  allows only a 64-bit `T`: `let n = 200; takeU8(n); n + n` would wrap in a `u8`, so `n` is a
  number and `takeU8(n)` needs `n as u8`. Only types the program names count: passing the
  local to an integer parameter of the JavaScript API (`s.slice(k)`) or using it as an index
  (`xs[k]`) leaves it a number, which the optimizer stores as an integer where that gives the
  same results. Locals used together (`x = y`, `x + y`) get one type. A local used both ways
  stays a number, and each use as `T` is an error that says why, with the fix: declare the
  type (`let i: i64 = 0`, which makes `/` on it integer division) or convert (`i as i64`).
- **The standard library hands you numbers**: its JavaScript API (the globals `Array`,
  `String`, `Map`, `Date`, `Math`, `fetch`, `URL`, …) gives numbers where JavaScript does:
  `xs.length`, `s.indexOf(t)`, `m.size`, `Date.now()`, `res.status` and the indexes of
  `entries()` and of array callbacks (inside the standard library they stay `usize`/`i64`).
  Velt's own modules (`velt:sqlite`, `velt:hash`, `velt:bigint`, …) and `compareTo` keep the
  integer types they declare. A number passed to an integer parameter of a standard library
  function converts like JS's `ToIntegerOrInfinity` (truncated; `NaN` is 0):
  `xs.slice(0, xs.length / 2)` takes the first half. A callback that the standard library
  expects to return a number, such as a `sort` comparator, may return an integer of any type:
  `items.sort((a, b) => a.id - b.id)` with `id: i64`.
- **Integers next to numbers**: `i8`..`i32`, `u8`..`u32` and `f32` convert to a number
  implicitly, because every value converts exactly (`k * 0.5` with `k: i32`). `i64`, `u64`,
  `isize` and `usize` need `as number`, and a number going into a declared integer needs `as T`
  (`n as usize`). Comparisons are exact: `i < xs.length` with `i: usize` compares the integers.
- **Declared integers keep Rust semantics**: they wrap at their width, `/` truncates
  (`7i64 / 2` is `3`), `/ 0` and `% 0` panic, `**` is integer power, and `%` takes the sign of the
  dividend. On numbers, `/ 0` is `Infinity`, `% 0` is `NaN` and `-4 % 2` is `-0`, as in JS.
  `Math.trunc(a / b)` on integers is one integer division.
- **`as`** converts with Rust semantics: floats truncate and saturate (`3.9 as i64` is `3`,
  `NaN as i64` is `0`), integers wrap (`300 as u8` is `44`, `-1 as u8` is `255`). So
  `x as i32` (saturates) and `x | 0` (wraps modulo 2^32, as in JS) are different operations.
  To any other type, `as` is TypeScript's type assertion and converts nothing: the expression is
  typed as if annotated with that type, so a literal takes its shape
  (`[["a", 1]] as [string, u8][]`, `[] as string[]`), and it must be a value of that type.
- **A float index** (`xs[i]` with `i: number`, `xs[Math.floor(n / 2)]`, `xs[parseInt(s)]`) must
  be a whole number at run time; anything else panics like an index out of bounds (JS reads
  `undefined`). Indexing with a quotient directly, `xs[n / 2]`, stays an error: write
  `Math.trunc(n / 2)`.
- **Bitwise operators on numbers are JS's 32-bit operators**: `| & ^ << >> ~` take ToInt32 of
  their operands (truncate, then wrap modulo 2^32 into the signed 32-bit range; `NaN` and
  ±Infinity give 0) and `>>>` takes ToUint32; shift counts are taken modulo 32, and the result is
  a number: `(a / 13) | 0` truncates, `-1 >>> 0` is `4294967295`, `1 << 32` is `1`. A product
  inside such an operand rounds like JS's double multiply past 2^53, so `(y * 0x2c1b3c6d) | 0`
  is Node's value; `Math.imul(y, 0x2c1b3c6d)` is the 32-bit wrapping product (one instruction).
  They compile to 32-bit integer instructions. Operands of a declared integer type keep their
  own width (`n >>> 3` with `n: i64` is a 64-bit shift), and so does a constant of two literals
  where an integer type is expected (`const m: u64 = 1 << 40`).
- **Printing follows Node**: `10`, `1.5`, `0.30000000000000004`, `1e+21`, `NaN`, `Infinity`.
  `console.log(-0)` prints `-0` (also inside arrays and objects); `` `${-0}` ``, `String(-0)`,
  `(-0).toString()` and `JSON.stringify(-0)` give `0`.

```ts
const a = 7;                              // a number
console.log(a / 2, a + 0.5, -a * 0);      // 3.5 7.5 -0
const n: i64 = 7;                         // a declared integer: integer arithmetic
console.log(n / 2, Math.trunc(a / 2));    // 3 3
let small: u8 = 250;
small += 10;                              // wraps: 4
console.log(small, n as number / 2, 300 as u8);  // 4 3.5 44
const h = 0x12345678;
console.log((h * 0x2c1b3c6d) | 0, Math.imul(h, 0x2c1b3c6d), -1 >>> 0); // -1019940576 -1019940584 4294967295

function collatz(start: i64): i64 {
  let x = start;
  let steps = 0;                          // returned as an i64: an i64
  while (x != 1) {
    x = x % 2 == 0 ? x / 2 : 3 * x + 1;
    steps++;
  }
  return steps;
}
console.log(collatz(27));                 // 111
```

### Migrating from inferred integers

Before the number model (#525), a local declared from an integer literal, a field declared from
one and every integer from the standard library were *inferred integers*: stored as `i64`, and
adapted to the integer types around them. They are numbers now, which changes code in these
ways:

- **Integer results past 2^53 round** instead of wrapping, `-0` prints as `-0`, `number / 0` is
  `Infinity` and `% 0` is `NaN` instead of panicking, and `**` on numbers is the double power:
  all as in Node.
- **A number where a declared integer is expected** is an error: `const k: usize = xs.length`
  becomes `const k = xs.length` (a number) or `xs.length as usize`, and `f(xs.length)` with
  `f(n: i64)` becomes `f(xs.length as i64)`. A local declared from a literal is fixed by the rule
  above when the function only uses it as that type; otherwise declare it (`let i: i64 = 0`).
- **An `i64` (or `u64`, `isize`, `usize`) next to a number** is an error: `total += c` with
  `total` a number and `c: i64` becomes `total += c as number`, or declare `total: i64`.
- **Integer division** on numbers is float division, as before: write `Math.trunc(a / b)`, which
  is one integer division when both operands are integers.
- **Fields declared from a literal** (`count = 0;`) are numbers; declare `count: i64 = 0` for an
  integer field.

## Strings

A `string` is an immutable value, like in JS: assign it, pass it, return it, store it, capture
it, take it out of a field, an array element, a `for...of` element or `Map.get`. The source stays
usable and no copy method is needed.

- `+` concatenates two strings; `s += x` appends to a variable or field in place when `s` holds
  the only reference to its text, growing it geometrically, so building a string in a loop costs
  time linear in its length. `s = s + x` and `` s = `${s}${x}` `` append the same way. Other
  copies of `s` never change.
- **No implicit conversion**: `"Total: " + 5` and `"a" + true` are compile errors. Build text
  with a template literal (`` `Total: ${n}` ``), which writes any value as `String(x)` does
  ([Lexical structure](lexical.md)).
- A string is a sequence of **UTF-16 code units**, as in JavaScript: `s.length` counts them, and
  every position (`slice`, `indexOf`, `charCodeAt`, `padStart`, regex offsets, `s[i]`) is a
  code-unit index. A character outside the Basic Multilingual Plane, such as an emoji, is two
  units (a surrogate pair): `"Zoë".length` is 3 and `"😀".length` is 2. A position may fall
  between the two halves of a pair; slicing there keeps the half as a lone surrogate
  (`"😀".slice(0, 1)` is `"\uD83D"`), and gluing the halves back together gives the pair again.
  Output writes a lone surrogate as U+FFFD. The byte size of a string in UTF-8 is
  `Buffer.byteLength(s)`.
- `s[i]` is `s.charAt(i)`: the code unit at `i` as a one-unit string, or `""` past the end (JS:
  `undefined`). `for (const c of s)` and `[...s]` iterate the characters (code points: a pair is
  one element), as JS's string iterator does; `s.split("")` gives code units.
- Methods: `slice substring indexOf lastIndexOf includes startsWith endsWith split trim
  trimStart trimEnd toUpperCase toLowerCase replace replaceAll repeat padStart padEnd charAt at
  charCodeAt`, plus `String.fromCharCode`, `parseInt`, `parseFloat` and `Number(s)`
  ([prelude](../std/prelude.md#strings)).
- `<`, `>` and `sort()` without a comparator compare by code units, as JS (`"～" < "😀"` is
  `false`); `==` compares content.
- Cost model: strings of up to 23 bytes (22 when they are not ASCII) are stored inline (no heap
  allocation); longer ones live in a reference-counted immutable buffer. A copy is 24 bytes
  plus, for a heap string, one count increment, and the compiler moves instead of copying at a
  last use. `s.clone()` compiles and is just a copy. Text is stored as UTF-8 (files, sockets and
  HTTP bodies need no conversion), with the code-unit count kept in the value: `length` is a
  load, and indexing ASCII text reads a byte. Indexing other text translates the position: a
  step from the last position of the same string, so a sequential loop over one or two strings
  at a time stays linear (each thread remembers its last two long non-ASCII strings), or a
  lookup in a table built for long strings plus a scan of at most 63 units (random access to
  long non-ASCII text is several times slower than in JS engines; in a long non-ASCII literal,
  which has no table, it scans from the closer end).
- A `slice` or `substring` of a heap string (and a piece from `trim` or `split`) that is at
  least a quarter of its buffer shares the buffer instead of copying (as JS engines' sliced
  strings do), so a parser that consumes its input with `rest = rest.slice(n)` runs in linear
  time. A smaller piece is copied, so a short slice never keeps a much larger string alive: live
  slices hold at most four times their own size.
- A string holds less than 2 GiB of text (more than JS engines allow). Making a longer one stops
  the program with `string too long` (`repeat` panics with JS's `RangeError` message instead).

```ts
function label(name: string, count: i64): string {
  let s = name;                   // a copy: `name` stays usable
  s += ":";
  return `${s} ${count} (${name.length} units, ${Buffer.byteLength(name)} bytes)`;
}

console.log(label("tea", 3), "a,b".split(","), "  x ".trim().padStart(3, "*"));
// tea: 3 (3 units, 3 bytes) [ 'a', 'b' ] **x
console.log(label("Zoë", 1), label("😀", 2));
// Zoë: 1 (3 units, 4 bytes) 😀: 2 (2 units, 4 bytes)
```

## Equality and comparison

- `==` and `===` are the same operator, as are `!=` and `!==`: there is no coercion, and the
  operands' types must overlap, as in TypeScript (`1 == "1"` is a compile error; an `i64`
  compared with an `i32` needs a cast). An interface value compares with a value of a class or
  struct that implements it, and a base class value with a subclass value.
- Numbers, bools and strings compare by value. Objects (class instances, arrays, maps,
  structs, object literals, interface and function values) compare by **identity**, like JS:
  `[1] == [1]` is `false`, and `a == b` is `true` when `b` refers to the same object as `a`.
  `T | null`, unions and tuples compare their parts that way. A `T | null` compares with a
  `T` (in either order) as if both were `T | null`: `null` equals no value. Likewise a union
  compares with one of its members (`string | number` with `number`): the two are equal when
  the union holds that member with an equal value, so the string `"3"` never equals the
  number `3`, as with `===` in JS.
- An interface value compares the object behind it: two `Shape` values of one class instance
  are equal. A function value is equal to its copies, and a named function to itself; each
  evaluation of an arrow or function expression is a new function, as in JS, also when it
  captures nothing (so `emitter.off(h)` finds the `h` given to `emitter.on(h)`, and two arrows
  made by one loop differ). `indexOf`, `includes` and `Map` keys agree with `==`. Comparing
  costs only the programs that do it: there, an arrow without captures gets an empty
  environment of its own when it is created, and a struct converted to an interface value
  that is compared is counted, so the interface value refers to it rather than to a copy.

  ```ts
  function main() {
    const h = () => console.log("h");
    const handlers = [h];
    const fresh: (() => void)[] = [];
    for (let i = 0; i < 2; i++) {
      fresh.push(() => console.log("h"));
    }
    console.log(handlers.indexOf(h), h === h, fresh[0] === fresh[1]); // 0 true false
  }
  ```
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
  see below); `switch` supports `case null`. The type of `x ?? d` is `x`'s non-null type
  when `d` converts to it, else `d`'s type when `x`'s non-null type converts to that, else
  their union, as in TypeScript: with `n: number | null`, `n ?? "none"` is a
  `number | string`.
- `x ??= d` assigns `d` when `x` is `null` and narrows `x` (likewise `x ||= d` and `x &&= d`).
  The target may not call a function yet (`m[key()] ??= v`): store the key in a variable first.
- `x!` is `x` known not to be `null` (TS's non-null assertion). TypeScript trusts it; Velt
  checks it: a `null` panics with `non-null assertion failed`.
- `a?: T` reads as `T | null`: an optional parameter `b?: T` is `b: T | null = null` (callers
  may leave it out or pass `null`; it cannot also have a default), and an optional field of a
  class, interface or object type starts as `null`, meaning absent: `JSON.stringify` leaves it
  out, and so does `console.log` in an object type, as JavaScript leaves out a missing key (a
  class shows it, as Node shows a class's optional field); a spread (`{ ...a, ...b }`) doesn't
  copy it over an earlier value. A `b: T | null` field holding `null` is written.
- A field declared `a?: T | null` in an object type keeps an absent key apart from a present
  `null`, as JavaScript does: it prints and serializes `null` when present, `JSON.parse` keeps
  the difference, and a spread copies a present `null` (`update(u, { deletedAt: null })`
  clears the field). In a class, such a field is still absent while it is `null`.
- An object literal may leave out any `T | null` field of an object type
  (`{ port: i64; host?: string }` accepts `{ port: 80 }`).
- In an object type, `?` is part of the type, as in TypeScript: `{ a?: string }` and
  `{ a: string | null }` read alike but are different types (the first may be absent), with no
  implicit conversion between them; copy with `{ ...x }`.
  Difference from TypeScript: TypeScript also accepts a `{ a: F | null }` where a
  `{ a?: F | null }` is expected; in Velt the two are different types (the second keeps a
  presence flag), so copy with `{ ...x }` there too.

```ts
type User = { name: string; deletedAt?: string | null };

function update(u: User, patch: Partial<User>): User {
  return { ...u, ...patch };
}

const u: User = { name: "ann", deletedAt: "2026-01-01" };
console.log(JSON.stringify(update(u, {}))); // {"name":"ann","deletedAt":"2026-01-01"}
console.log(JSON.stringify(update(u, { deletedAt: null }))); // {"name":"ann","deletedAt":null}
const v: User = { name: "bo" };
console.log(JSON.stringify(v)); // {"name":"bo"}
console.log(update(v, { deletedAt: null })); // { name: 'bo', deletedAt: null }
```

- `JSON.parse<T>` treats an absent key like an explicit `null` (a `T | null` field may be
  missing), except for an `a?: T | null` field of an object type, which records whether the key
  was there. A `JsonValue` always tells them apart: `v.has("a")` vs `v.get("a")?.isNull()`.
- `x?.a.b` short-circuits the rest of the chain like TypeScript (null when `x` is null; `.b` is
  never evaluated). Parentheses end a chain: `(x?.a).b` needs `x?.a` to be non-null.
- Narrowing applies to locals and to field paths of locals (`this.x`, `node.left`), like
  TypeScript; assigning a non-null value narrows too. A narrowed field is re-checked when read,
  so a call that set it to `null` in between panics instead of reading `null`. Inside a
  closure, a variable narrowed where the closure is created stays narrowed.
- A variable that a closure assigns is not narrowed (by any check: `!= null`, `typeof`,
  `instanceof`, …), where TypeScript keeps the narrowing: a call between the check and the use
  may run the closure, and then the variable no longer holds what was checked. Test a `const`
  copy instead, which nothing can reassign; the error at such a use says so:

  ```ts
  class Conn {
    send(msg: string): string { return `sent ${msg}`; }
  }

  let conn: Conn | null = new Conn();
  const close = () => { conn = null; };
  const c = conn;                 // `if (conn !== null) { conn.send(…) }` is an error here
  if (c !== null) {
    close();
    console.log(c.send("bye"));   // sent bye
  }
  ```
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
  - `x instanceof C` matches members whose class is `C` or a subclass. A member of a base
    class of `C`, or an interface value, is tested at run time and narrows to `C`
    ([downcasts](classes.md#instanceof-downcasts)).
  - `x == literal` / `x != literal` selects the literal's member.
  - Conditions of `if`, `while`, `&&`, `||`, `!`, ternaries and early exits narrow a local
    until it is reassigned; `switch` narrows each case ([`switch`](control-flow.md#switch)).
    A local that a closure assigns is not narrowed ([Null](#null)).
- Printing and template literals show the active member's value. A union with a member that
  cannot be printed (a closure) prints once a test has narrowed it to members that can
  (`typeof v !== "function"`). `JSON.stringify` works on
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
  `class Node { kind: "node"; kids: Tree[] }`), because an alias that is a union cannot refer
  to itself.
- Payload enums and `match` do not exist; both are errors with a hint to use a discriminated
  union.

## Intersection types

`A & B` is the object type with the fields of both `A` and `B`, as in TypeScript. The parts are
object types: anonymous ones, aliases of them, interfaces with only fields, results of utility
types, other intersections, and unions of these. `&` binds tighter than `|`
(`A & B | C` is `(A & B) | C`), and a leading `&` is allowed like a leading `|`.

```ts
type Named = { name: string };
type Aged = { age: number };
type Person = Named & Aged; // { name: string; age: number }

interface HasId {
  id: string;
}
type Entity = HasId & { createdAt: number };
type WithMeta<T> = T & { meta: string };

const ada: Person = { name: "Ada", age: 36 };
console.log(JSON.stringify(ada)); // {"name":"Ada","age":36}
const e: Entity = { id: "e1", createdAt: 1700 };
const n: Named = { name: "Grace" };
const grace: Named & Aged = { ...n, age: 45 }; // spread builds one from the parts
const w: WithMeta<{ x: number }> = { x: 3, meta: "m" };
console.log(e.id, grace.age, w.meta); // e1 45 m
```

- **Fields** are the first part's, then the next part's new ones: the key order of
  `{ ...a, ...b }`, so printing and `JSON.stringify` match Node.
- **A field in several parts** gets the intersection of its types: the same type stays, object
  types merge (`{ p: { x } } & { p: { y } }` has `p: { x; y }`), a literal type and its base
  type give the literal, and union members that have no value in common drop out. The field is
  optional only when it is optional in every part, and `readonly` only when it is `readonly` in
  every part that has it (both as in TypeScript).
- **Unions distribute**: `(Circle | Square) & { id: string }` is
  `(Circle & { id: string }) | (Square & { id: string })`, a
  [discriminated union](#discriminated-unions) that narrows as usual; `Shape & { kind: "circle" }`
  keeps only the circle member, and `(A | null) & B` is `A & B`.
- The result is an ordinary object type: there is no cost at run time, and `A & B` is the same
  type as the object type with those fields written out in the same order.
- **Conversions**: a value converts to an object type whose fields it has, by name, as in
  TypeScript: `A & B` where an `A` or a `B` is expected, `B & A` (or `{ b; a }`) where `A & B`
  is expected, and to a type with an optional field the value lacks (it is absent). Object
  types have fixed layouts, so the conversion builds a new object of the expected type holding
  the same field values: one allocation and a copy of each field, paid where a program converts
  (nested objects and arrays are shared, not copied). TypeScript passes the same object, so
  where the program could tell the difference, the conversion is an error with the fix (build
  the object from its fields, `{ a: ab.a }`, or take the wider type): when the program assigns
  a copied field of either type (`a.a += 1` on an `A` anywhere), or an optional field the
  value lacks (`x.c = "s"` on an `{ a: number; c?: string }`), compares values of the
  expected type with `===` (also as `A | null` or another union holding it), prints, serializes or lists the keys of a value holding the
  expected type, or spreads a value of the expected type (`{ ...x, c: 3 }`; Node would show
  or copy the original's fields, in its order), directly or in generic code it calls (`xs.indexOf(x)` and `xs.includes(x)` compare with `===`). An array
  converts element by element only when fresh, like [wider element
  types](#objects-arrays-tuples-and-maps): `const ns: Named[] = roster();` for a `roster()`
  that returns a new `(Named & Scored)[]`; copy another one with
  `xs.map((p) => ({ name: p.name }))`.
- An alias may refer to itself through `&` when its parts are object types written out:
  `type Tree = { kids: Tree[] } & { v: number }` is the interface with the fields `kids` and
  `v` (an alias that names itself otherwise is an error, as is one whose parts share a field
  name).

```ts
type Named = { name: string };
type Scored = { score: number };

function greet(n: Named): string {
  return `hi ${n.name}`;
}

function rank(p: Scored & Named): string {
  return `${p.name}: ${p.score}`;
}

type Tree = { kids: Tree[] } & { v: number };

const ken: Named & Scored = { name: "Ken", score: 7 };
console.log(greet(ken), rank(ken)); // hi Ken Ken: 7
const t: Tree = { kids: [{ kids: [], v: 2 }], v: 1 };
console.log(t.kids[0].v); // 2
```

Differences from TypeScript, each a compile error with a note on what to write instead:

- When the parts have no value in common (`{ k: string } & { k: number }`, or two different
  discriminants), TypeScript makes the type, or the field, `never`; Velt reports
  ``no value has type `…`: field `k` is `string` in one part and `f64` in another``.
- Classes and structs are not parts (Velt classes are nominal, not structural): use
  `Pick<C, …>` or a field-only interface. Interfaces with methods, arrays and function types
  (overloads) are not parts either; `T extends A & B` stays a bound on two interfaces.
- A part that is a type parameter (`function merge<T, U>(t: T, u: U): T & U`) is not supported
  yet (#350). A generic alias works, since each use has concrete type arguments.
- Interface declarations are not merged (#652): declare an interface once, or name the
  combination with `&`.

```ts error
type Conflict = { k: string } & { k: number }; // error: no value has type ...
```

### Branded types

A primitive `&` an object type (`string & { __brand: "UserId" }`) is a **branded type**: a
nominal alias of the primitive, with no cost at run time. `x as UserId` brands a value; a
branded value works wherever its primitive does (members, operators, `${}`, arguments, map
keys); a plain `string`, or another brand of it, does not convert to the brand.

```ts
type UserId = string & { __brand: "UserId" };
type Cents = number & { readonly __unit: "cents" };

function greet(id: UserId): string {
  return `user ${id}`;
}

const id = "u-42" as UserId;
console.log(greet(id), id.length, id.toUpperCase()); // user u-42 4 U-42
const price = 449 as Cents;
console.log(price / 100); // 4.49
```

```ts error
type UserId = string & { __brand: "UserId" };
const id: UserId = "u-1"; // error: a plain `string` does not convert to it; brand a value with `x as UserId`
```

### Indexed access types

`T["k"]` is the type of field `k` of a concrete object type `T`, and `T["a" | "b"]` the union
of the fields' types, as in TypeScript: `Person["name"]` is `string`. A key that is not a field
is an error. The key is a string literal type or a union of them; on a type parameter it is not
supported yet (#350).

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
- **Quoted property names** work as in TypeScript, for names that are not identifiers:
  `type Headers = { "content-type": string }`, `{ "a-b": 1 }`, `interface A { "data-id": string }`
  and `const { "a-b": n } = o`. `o["a-b"]` (or `` o[`a-b`] ``) reads the field; with any string
  literal, `o["name"]` is the same as `o.name`. `console.log` quotes the names that are not
  identifiers, as Node does (`{ 'a-b': 1 }`), and `JSON.stringify` writes them as given. Names
  beginning with `#` or `[Symbol.`, `"__proto__"` (it sets the prototype in JavaScript) and
  quoted method names are not supported. Parameter destructuring isn't supported yet, so quoted
  names in it aren't either.
- **Key order** of an object type is JavaScript's: field names that are array indices (`"0"`,
  `"404"`: canonical, up to 2^32 - 2) come first, ascending, then the others in declaration
  order. `console.log`, `JSON.stringify` and `Object.keys` all follow it. A `Record` and a
  `JsonValue` keep insertion order for every key (#756).
- **Generic object types** are structural, as in TypeScript: an instance is the object type it
  spells out, so with `type Box<T> = { v: T }`, `Box<string>` *is* `{ v: string }`, and so is
  the instance of a generic interface with only fields.

```ts
type Box<T> = { v: T };

function box<T>(v: T): Box<T> {
  return { v };
}

const b: { v: string } = box("hi"); // `Box<string>` is `{ v: string }`
console.log(b.v); // hi
```

- **`readonly` fields**: in `{ readonly id: i64; name: string }`, assigning `id` is an error
  (``cannot assign to `id`: it is a readonly field``); like TypeScript's, the check is shallow
  (`u.tags.push(x)` is fine). A value converts between a type and the same type without
  `readonly`, in both directions, and stays the same object.
- **Utility types** build an object type from a concrete one (an object type, an interface
  with only fields, or a class or struct, whose public fields are used):
  `Partial<T>` (every field optional), `Required<T>` (no field optional),
  `Readonly<T>` (every field `readonly`), `Pick<T, K>` (only the fields named in `K`) and
  `Omit<T, K>` (every other field). `K` is a string literal type or a union of them
  (`"id" | "email"`). In `Pick` a name that is not a field is an error; in `Omit` it is a
  warning, as TypeScript accepts it (so `type WithoutChildren<P> = Omit<P, "children">` works
  on types without `children`). The results are ordinary object types: `Pick<User, "name">`
  *is* `{ name: string }`, and declaration order doesn't matter. As in TypeScript, `Required`
  removes only the `?`: a field written `a: T | null`, or `a?: T | null`, stays nullable.
  Differences from TypeScript: an operator on a type parameter (`Partial<T>` in a generic
  function) is not supported yet (#350); and a type can't apply one to itself in its own fields
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
  `[x, ...xs]` builds a new array, converting each element to the expected element type
  (integer elements spread into a `number[]`, `const ns: Named[] = [...cs]`). Spread
  arguments, `f(...xs)`, fill a rest parameter ([Functions](functions.md)). Whatever `for...of`
  takes can be spread into an array or a rest parameter too: `[..."héllo"]` (characters),
  `[...map]` (entries), `[...gen()]`, `Math.max(...set)`
  ([Consuming an iterable](control-flow.md#consuming-an-iterable)).
- **Wider element types**: an array, object type or generic class converts to the same type
  with wider elements (`C[]` to `Named[]` for a class `C implements Named`, `i64[]` to
  `(i64 | null)[]`, `Box<C>` to `Box<Named>`) only when the value is **fresh**: a literal, a
  `new` expression, or the result of a call of a function that returns a new value on every
  path (a literal, `new`, such a call, or a local it builds and returns without storing or
  passing it anywhere, as `map` and `filter` do). The conversion builds a new value with each
  element converted. A call that may return a value something else still holds (a getter
  returning a field) is an error with the same fix as below, and so is a conversion in a field
  initializer or a default value for now. TypeScript also converts an existing array, which
  is unsound: storing a `Named` that is not a `C` through the `Named[]` would put it into the
  `C[]`. Velt reports that and
  suggests a copy, `[...cs]` or `cs.map((x): Named => x)`. A generic class converts only when
  it has no base class, no subclasses and no `[Symbol.dispose]()`; the new object shares the
  old one's field values.

  ```ts
  interface Named {
    name(): string;
  }

  class C implements Named {
    name(): string {
      return "c";
    }
  }

  function make(): C[] {
    return [new C()];
  }

  function main() {
    const ns: Named[] = make(); // a fresh C[]: converted
    const cs = make();
    const copy: Named[] = [...cs]; // `const ns2: Named[] = cs;` is an error
    console.log(ns.length, copy.length); // 1 1
  }
  ```
- **Destructuring**: `const [a, b] = pair;`, `const [head, ...rest] = xs;`,
  `const { a, b } = obj;`, and `for (const [k, v] of map)`. Array destructuring checks the
  length like indexing: a shorter array panics with the same `index out of bounds` message.
  A string, a map or an iterable is destructured like in JS (`const [first, ...rest] = "abc"`):
  `const [a, b] = gen()` takes two values and closes the iterator; one that has fewer values
  panics like a short array, unless the pattern gives defaults. Nested patterns work too
  (`const [[a, b], [c]] = [gen(), gen()]`). An object pattern reads properties, as in JS:
  `const { length } = xs;` and `const { length: n } = "abcd";` read the length, and a getter
  is called (`const { area } = rect;`).
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
  `getOrInsert(k, () => v)`. Keys: numbers, `bool`, `string`, class instances, interface and
  function values (by identity, as `==` compares them),
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
  and `Object.entries(r)` return arrays in insertion order, also for array-index keys, which
  JavaScript lists first (#756) (`for (const [k, v] of
  Object.entries(r))`). Given an object literal, `Object.values` and `Object.entries` read it
  as a `Record<string, V>`, so its values need one type. `Object.keys` accepts any object, as
  in TypeScript: an object literal or object type (`Object.keys({ a: 1, b: "x" })` is `["a",
  "b"]`), a struct, or a class instance, whose fields it lists in declaration order (base class
  fields first, `private` ones too; not ES private `#x` fields, static fields or methods). A struct's optional field is
  listed only when it is not `null`. On a class with subclasses it lists the fields of the
  object's actual class (a `Shape` holding a `Rect` lists the `Rect` fields too), and on an
  interface value those of the class it holds (an interface also implemented by a struct is an
  error: struct values carry no class). `console.log` and `JSON` treat a record as an object. A class
  cannot `extends` a `Record` (its constructor would leave a closed record without its keys);
  hold one in a field instead. A literal for an enum-keyed record is not supported yet.
- `JSON.stringify(x)` / `JSON.parse<T>(s)` are generated at compile time for numbers, bools,
  strings, literal types, arrays, tuples, enums, nullable values, `Map<string, V>`,
  `Record<K, V>`, structs, classes and anonymous objects ([`velt:json`](../std/json.md)).
  `JSON.stringify` writes a class's or struct's `private` fields, as Node does, and skips ES
  private fields (`#x`). `JSON.parse<T>` cannot build a type with a `private` or `#` field (a
  compile error naming the field: decoding does not run the constructor). A type holding a std
  type's private state (a runtime handle) has no JSON form in either direction, so handles
  can't be forged from JSON.

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
