# Types

| Type | Meaning |
|---|---|
| `i8 i16 i32 i64 isize`, `u8 u16 u32 u64 usize` | fixed-width integers |
| `f32 f64`; `number` | floats; `number` is `f64`, a JavaScript number ([Numbers](#numbers)) |
| `boolean`, `bool` | `true` / `false`; one type with two names ([Booleans](#booleans)) |
| `string` | immutable text, indexed in UTF-16 code units (stored as UTF-8), a value ([Strings](#strings)) |
| `symbol`, `unique symbol` | unique values compared by identity, also naming members ([Symbols](#symbols)) |
| `u8x16`, `i32x4`, `f32x4`, `f64x2`, …; `mask8x16`, … | 128-bit SIMD vectors and the masks comparing them gives ([SIMD vectors](#simd-vectors)) |
| `void`, `never` | no value; no possible value ([`switch`](control-flow.md#switch)) |
| `T[]` | growable array |
| `[A, B]` | tuple |
| `Map<K, V>` | insertion-ordered hash map |
| `{ a: T; b: U }` | anonymous object type |
| classes, structs, interfaces | [Classes](classes.md) |
| `A \| B`, `T \| null` | [unions](#union-types), [nullable values](#null-and-undefined) |
| `A & B`, `T["k"]` | [intersections of object types](#intersection-types), [branded types](#branded-types), [indexed access](#indexed-access-types) |
| `"up"`, `42`, `true` | [literal types](#literal-types) |
| `(x: T) => U` | function values: closures and named functions ([Functions](functions.md)) |
| `Promise<T>`, `shared<T>`, `Mutex<T>` | async results and thread-safe shared values ([Async](async.md)) |

Types are required on function parameters. A missing return type is inferred from the
function's `return`s ([Return types](functions.md#return-types)), and everything else is
inferred too. `type Name = …` declares an alias; it is checked even where nothing uses it. An
alias may refer to itself only when it is an object type, or an intersection of object types,
written out (`type Tree = { kids: Tree[]; v: number }`): it is then the interface with those
fields ([Intersection types](#intersection-types)). `any` and `unknown` hold a value of any
type, checked at run time ([any and unknown](#any-and-unknown)); dynamic JSON is `JsonValue`
([`velt:json`](../std/json.md)).

The names TypeScript does not have (`i8`…`i64`, `isize`, `u8`…`u64`, `usize`, `f32`, `f64`,
`bool`) are like library globals: a type alias, class, interface or enum of the module with
one of these names, or one it imports, takes their place there, as a local `Map` would in
TypeScript. So TypeScript code that declares `type u8 = number` compiles and behaves as in
Node. A module that declares none of them has Velt's types. TypeScript's own names (`number`,
`boolean`, `string`, …) cannot be declared.

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
  As in TypeScript, `as` and `satisfies` take a whole type: `x as A | B` is a union and
  `x as A & B` an intersection (a leading `|` may start the type on the next line), so a
  bitwise operator after a cast needs parentheses: `(x as i32) | y`. A number literal after
  `|` in a cast type (`x as i32 | 1`) is an error that shows both spellings, `(x as i32) | 1`
  and `x as (i32 | 1)`, since earlier versions of Velt read it as a bitwise or.
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
- `+` with a string on one side and a number or boolean (or one of those or `null`) on the
  other concatenates, as in JS: `"Total: " + 5` is `"Total: 5"`, `"a" + true` is `"atrue"` and
  `s += n` appends. The number is written as `String(n)` and `console.log` write it (`1.5`,
  `1e+21`, `NaN`, `Infinity`, `0` for `-0`). Any other value next to a string is a compile
  error; build that text with a template literal (`` `Total: ${xs}` ``).
- `String(x)` converts any value that can be printed to its text, exactly as `` `${x}` ``
  writes it (`String(1.5)` is `"1.5"`, `String(null)` is `"null"`, `String([1, 2])` is
  `"1,2"`). `Number(x)` converts a string (as JS: the whole trimmed string, `""` is `0`, else
  `NaN`), a number or a boolean (`1` / `0`); `Boolean(x)` is `x`'s
  [truthiness](variables.md#conditions-truthiness). They are called as functions, not as values
  (`xs.map(String)` is not supported yet: write `xs.map((x) => String(x))`).
- A template literal writes `${x}` as JS's `String(x)` does ([Lexical structure](lexical.md)):
  an array's elements joined with `,` (`${[1, 2]}` is `1,2`, nested arrays the same way, `null`
  elements as empty text, a class instance through its `toString()`, another object as
  `[object Object]`, a map as `[object Map]`, a set as `[object Set]`), and a class instance
  through its `toString()`, else as `[object Object]`. An array whose elements JS writes with a
  method Velt cannot call there (a struct's `toString()`, an `Error`, a `RegExp`) is a compile
  error in a template literal, in `join` and in `toString()`: write
  `` `${xs.map((x) => x.toString()).join(",")}` ``.
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
  charCodeAt`, plus `String.fromCharCode`, `parseInt`, `parseFloat`, `String(x)` and `Number(s)`
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

## Symbols

`Symbol(description?)` makes a new `symbol`, a value different from every other one and
compared by identity; `Symbol.for(key)` returns the registry's symbol for `key` (the same one on
every call), and `Symbol.keyFor(s)` its key. A symbol has a `description` (`null` where Node
says `undefined`) and `toString()` (`Symbol(description)`); `typeof s` is `"symbol"`, and
`console.log` shows it as Node does. Symbols are `Map` keys and `Set` members. The well-known
symbols are values too (`Symbol.iterator`, `Symbol.asyncIterator`, `Symbol.toStringTag`, …).
Converting a symbol to a string implicitly (`${s}`, `"k" + s`, also of a `string | symbol`) is an
error, as JavaScript throws a `TypeError` there; `String(s)` is allowed. Where the compiler
cannot see the symbol, Velt panics with that `TypeError` (exit code 101) instead: joining an
array or tuple that holds one (`[s].join()`, `String([s])`, `${[s]}`, `${pair}` of a
`[string, symbol]`, also of a `(string | symbol)[]` that holds one), and a template literal part
whose type is a type parameter that is `symbol` or a union holding a symbol (`` `v=${x}` `` with
`x: T`, called with a symbol; the union's other members are written as usual). Unlike Node's
`TypeError`, this panic cannot be caught with `try`/`catch`. The API is [`Symbol`](../std/symbol.md).

A symbol a module constant holds names a member: `[KEY]: T` in an interface, an object type or
a class, `{ [KEY]: v }` in an object literal, `o[KEY]` and `KEY in o`. The constant is
initialized with `Symbol("...")` or `Symbol.for("...")` (TypeScript's `unique symbol`;
`const KEY: unique symbol = Symbol("k")` declares the same), or with another such constant, and
may be imported. Velt objects have fixed fields, so the compiler resolves these members while
compiling: reading `o[KEY]` costs what reading a named field does. A symbol key known only at
run time (a symbol made in a function) is an error: use a `Map`. In an object literal, a string
key known only at run time (`{ [o.tag]: v }`, `{ [name]: v }` with a local `name`) makes the
object a `Record<string, V>`, as TypeScript gives it a string index signature. A module constant
holding a string literal names that field the same way (`const NAME = "name"`, `[NAME]: T`).
A class or interface may declare a method, getter or setter with such a key (`[KEY]() {}`,
`get [KEY]()`) or a well-known symbol key (`get [Symbol.toStringTag]()`); `o[KEY]`, `o[KEY]()`,
`o[KEY] = v` and `KEY in o` find it. A quoted key is an ordinary string key, also one that reads
like a symbol (`"[x]"`, `"[Symbol(k)]"`); only names starting with `"[Symbol.` or with the
noncharacter U+FFFF are reserved (Velt names symbol-keyed members with them). `console.log`
shows a symbol-keyed member after the others as `[Symbol(description)]: v`, and
`JSON.stringify` and `Object.keys` leave it out, as in Node; `JSON.parse` never fills one,
whatever keys the text has (a required symbol-keyed field makes the decode fail).

`KEY in o` is `true` when `o`'s type declares the member, `false` when it doesn't, and for an
optional member (`[KEY]?: T` in an object type or interface) whether it is present, tested at
run time (a class's optional field is always present, as TypeScript-compiled classes define
it). On a union it answers for the member `o` holds, and in an `if` it narrows the union to the
members that may have the key, as TypeScript does. `Symbol.iterator in o` and
`Symbol.asyncIterator in o` answer from the type's methods: arrays, `Map`s, `Set`s and generators
have `Symbol.iterator`, async generators `Symbol.asyncIterator`, and `Symbol.asyncIterator in g()`
on a generator is `false`. Another well-known symbol `in` a built-in object
(`Symbol.toStringTag in m`) is an error rather than a guess.

```ts
const VNODE: unique symbol = Symbol("vnode");
const MODEL = Symbol.for("app.model");

interface VNode {
  type: string;
  [VNODE]: true;
}

function isVNode(v: VNode | string): boolean {
  return typeof v !== "string" && v[VNODE] === true;
}

function main() {
  const n: VNode = { type: "div", [VNODE]: true };
  console.log(n, isVNode(n), VNODE in n); // { type: 'div', [Symbol(vnode)]: true } true true
  console.log(JSON.stringify(n), MODEL === Symbol.for("app.model")); // {"type":"div"} true
  const tokens = new Map<symbol, string>();
  tokens.set(MODEL, "model");
  console.log(tokens.get(Symbol.for("app.model")), typeof VNODE); // model symbol
}

type Cell = { value: number; [VNODE]?: boolean };

function describe(c: Cell | VNode): string {
  if (VNODE in c) {
    return "vnode or marked cell";
  }
  return `cell ${c.value}`;
}

function check() {
  console.log(describe({ value: 1 }), describe({ value: 2, [VNODE]: true })); // cell 1 vnode or marked cell
}
```

A module constant initialized with `Symbol(...)` or `Symbol.for("...")` is one symbol for the
whole program, made while compiling (module constants are otherwise evaluated at each use), so
reading it costs no call; `Symbol.for(k)` at run time gives that same symbol for its key. A symbol `Symbol(...)` makes at run time lives
until the program ends. **Planned**: a symbol brand added with `Object.defineProperty(o, KEY,
…)` as an optional symbol-keyed field of `o`'s type
([#922](https://github.com/velt-lang/velt/issues/922)), and `PropertyKey` (`string | symbol`)
for `Reflect` and proxies ([#823](https://github.com/velt-lang/velt/issues/823)).

## SIMD vectors

A SIMD vector holds several numbers of one type, its *lanes*, and an operation on it works on
all of them with one CPU instruction: a `u8x16` is sixteen `u8`, an `f32x4` four `f32`. The
types are `i8x16 u8x16 i16x8 u16x8 i32x4 u32x4 i64x2 u64x2 f32x4 f64x2`, each 128 bits, which
every x86-64 and arm64 CPU has: the same source compiles everywhere, with no CPU flag, in debug
and release builds. On WebAssembly they are `v128` values: WebAssembly SIMD is part of Velt's
WebAssembly baseline, and a program that uses vectors needs an engine with it (every current
browser and runtime). They are Velt's own, and not global: a module imports the ones it names
from [`velt:simd`](../std/simd.md) (`import { u8x16 } from "velt:simd";`), so a module that
does not is free to use these names for its own types, as TypeScript code may.

A vector is a value like a number: it is copied when assigned or passed, lives in a register,
and can be a field, an array element, `T | null` or a `Map` value. Its operations are methods
([`simd`](../std/simd.md)), not operators, and it has no `==`, truthiness, printed or JSON form:
comparing two vectors with `eq`, `lt`, … gives a *mask* (`mask8x16` for a `u8x16`), one lane
per lane, set or not, which `any()`, `all()`, `first()`, `count()` and `bitmask()` turn into a
`boolean` or a number and `select` into a choice between two vectors.

```ts
import { u8x16 } from "velt:simd";

// The position of the first `b` in `d`, sixteen bytes at a time.
function indexOf(d: u8[], b: u8): i64 {
  const needle = u8x16.splat(b);
  let i: usize = 0;
  while (u8x16.fits(d, i)) {
    const found = u8x16.load(d, i).eq(needle);
    if (found.any()) {
      return (i + found.first()) as i64;
    }
    i += 16;
  }
  while (i < d.length) {
    if (d[i] == b) {
      return i as i64;
    }
    i++;
  }
  return -1;
}

function main() {
  const text: u8[] = Buffer.alloc(100);
  text[70] = 33;
  console.log(indexOf(text, 33)); // 70
}
```

`load` and `store` panic with a RangeError unless the whole vector is inside the array, and
`lane(i)` unless `i` is a lane. `fits(xs, i)` is the test they make: in a loop that runs while
it holds, as above, nothing is left for them to check.
A loop over plain numbers is often compiled to the same instructions without any of this
(summing an array, adding two arrays); vectors are for the loops it cannot do that for, such as
one that stops early, or a sum of floats, whose order it must keep.

## Equality and comparison

- `==` and `===` (and `!=` and `!==`) differ only against `null` and `undefined`, as in
  JavaScript: `x == null` and `x == undefined` match either nothing, `===` only the one written
  ([Null and undefined](#null-and-undefined)). Everywhere else they are the same operator: there
  is no coercion, and the operands' types must overlap, as in TypeScript (`1 == "1"` is a compile error; an `i64`
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

## Null and undefined

`null` and `undefined` are two values, as in JavaScript. A value that may be absent has type
`T | null`, `T | undefined` or `T | null | undefined` (`A | B | undefined` for a union), stored
without an extra allocation where possible; `T | null` and `T | undefined` take the same space,
and `T | null | undefined` holds a `T | null` with one more flag. `undefined` behaves as in Node:

```ts
function find(xs: string[], s: string): string | undefined {
  return xs.includes(s) ? s : undefined;
}

const a = find(["x"], "y");
console.log(a, typeof a, a === undefined, a == null); // undefined undefined true true
console.log(a ?? "none", `${a}`, [a]);                 // none undefined [ undefined ]
console.log(JSON.stringify({ a, b: 1 }), JSON.stringify([a]));    // {"b":1} [null]
const v: number | undefined = void console.log("ran"); // `void e` evaluates `e`: undefined
```

A `T | null | undefined` keeps the two apart: `x === null` and `x === undefined` each test
one, `x == null` both, and `typeof`, printing and `JSON.stringify` see which one it holds.
`x !== undefined` narrows it to `T | null`, and both tests (or `x != null`, or truthiness)
narrow it to `T`. `undefined` alone is a type too (its only value is `undefined`), and an
`undefined` with no type to take has it (`const u = undefined`, `[1, undefined]` is a
`(number | undefined)[]`). A `let x: T | undefined;` without an initializer starts as
`undefined`.

```ts
function show(x: string | null | undefined): string {
  if (x === undefined) return "undefined";
  if (x === null) return "null";
  return x.toUpperCase();
}

const xs: (string | null | undefined)[] = ["a", null, undefined];
console.log(xs.map(show), xs.map((x) => typeof x)); // [ 'A', 'null', 'undefined' ] [ 'string', 'object', 'undefined' ]
console.log(JSON.stringify({ a: xs[1], b: xs[2] }), xs[2] ?? "d"); // {"a":null} d
let later: number | undefined;
console.log(later, [1, undefined]); // undefined [ 1, undefined ]
```

`x == null` and `x == undefined` match either nothing; `x === undefined` tests a
`T | undefined` for its nothing, and `x === null` on a `T | undefined` is `false` (`!==`: `true`)
and narrows nothing, as in Node; `switch` compares like `===`. `typeof x === "undefined"`
narrows, `?.` on a value that may be `undefined` gives `undefined` (a `T | null` result
becomes `T | null | undefined`), and `JSON.stringify` leaves out a field
holding `undefined` and writes an array element as `null`. A destructuring default applies where
the value is `undefined` (an `undefined` element or field, an absent optional field, a too short
array), never to a `null` a field or element is declared to hold: `const { a = 1 } = o` keeps
the `null` of a field `a: T | null`, and of an optional `a?: T | null` that is present. `sort(cmp)`
and `toSorted(cmp)` on a `(T | undefined)[]` never pass an `undefined` element to `cmp` and put
those elements last. `return undefined;` in a `void` function is `return;`, and `return void e;` (or an arrow body `(): void => void e`) is `e;` then `return;`. `T | void` is
`T | undefined` (a `void` function's value is `undefined`), so a callback typed
`(e: Error) => boolean | void` may return a `boolean` or nothing. A function whose result type
allows `undefined` returns it from a bare `return;` and when it reaches its end; one whose result
type is inferred, that returns values and also `return;` or can reach its end, returns
`T | undefined`, as in TypeScript. `null` where `T | undefined` is expected, and `undefined` where
`T | null` is, are errors (TS2322), also for a `T | undefined` value where a `T | null` is
expected and the reverse.

Not supported yet, each with a precise error ([issue #930](https://github.com/velt-lang/velt/issues/930)):
`null | undefined` alone as a type, `JSON.stringify`
of a value that may itself be `undefined` and `JSON.parse` into a type holding `undefined`;
`x === undefined` and `typeof x === "undefined"` on a `T | null` (its `null` may be JavaScript's
`undefined`, see below; `x == null` tests for either) and `case null` / `case undefined` with
the nothing the type lacks; `as` between `T | undefined` and `T | null`; a parameter default
other than `undefined` on a parameter whose type allows `undefined` (JavaScript applies it to
a passed `undefined` too); and a type argument whose nothing differs from one the generic keeps
of it, directly or through what it calls (a `T | undefined` for `Map.set`,
`Array.prototype.find` or a generic function holding a `T | null`), which includes a generic
alias applied that way (`type Nullable<T> = T | null` as `Nullable<number | undefined>`). The
other way round is supported: a generic holding a `U | undefined` at `U = T | null` holds a
`T | null | undefined`. Also not supported yet, although TypeScript accepts them:

- `undefined` (the literal, `void 0` or a value that may be `undefined`) passed for an optional
  parameter or one with a default (`f(undefined)` for `function f(x = 5)` or `f(a?: number)`):
  leave the argument out;
- `undefined` as the value of an optional field (`{ a: undefined }` for `a?: number`): leave the
  field out (also for a conditional with an `undefined` branch, `c ? v : undefined`, given
  for either);
- a `T | null` from the standard library where a `T | undefined` is expected
  (`return m.get(k);` in a function returning `number | undefined`): write `m.get(k) ?? undefined`;
- a type argument that allows `undefined` for generic code that sorts an array of its type
  parameter (`xs.sort(cmp)` inside `function f<T>(xs: T[], …)`): an `undefined` element would
  reach the comparator;
- `JSON.stringify` of a value holding a class whose `toJSON()` may return `undefined` (its
  own or a subclass's);
- a destructuring default for an optional **class** field declared `T | null`
  (`class C { b?: number | null }`, `const { b = 6 } = c`): a class does not keep an unset field
  apart from one set to `null`.

### Silent differences until phase 3

These compile and print or test differently from Node, with no error, until later phases of
[issue #930](https://github.com/velt-lang/velt/issues/930) give them `undefined`:

- a missing key of a `Map`: `m.get("z")` is `null` where Node gives `undefined` (`console.log`
  prints `null`, `typeof` gives `"object"`), whatever the value type (`Map<string, number>`,
  `Map<string, number | null>`);
- a missing key of a `Record`: `r["z"]` on a `Record<string, number>` or
  `Record<string, number | null>` is `null` likewise;
- an absent optional field or a left-out optional parameter (`a?: T`), `?.` on a `T | null`
  and the rest of the standard library (`find`, `at`, `shift`, …) give `null` where Node gives
  `undefined` (phases 2 and 3); `a?: T | undefined` is reported as a type with both nothings.

`x == null` tests for either nothing and `x ?? d` treats both alike, so code written with them
behaves as in Node.

### Optional values and `null`

Optional fields and parameters (`a?: T`), `?.` on a `T | null` and the standard library use
`null` for now:

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
  - `typeof x === "string" | "number" | "boolean" | "symbol" | "object" | "function"` (and
    `!==`): all number types are `"number"`; classes, structs, arrays, maps and `null` are
    `"object"`; closures are `"function"`. An impossible tag is an error.
  - `x instanceof C` matches members whose class is `C` or a subclass. A member of a base
    class of `C`, or an interface value, is tested at run time and narrows to `C`
    ([downcasts](classes.md#instanceof-downcasts)). `x instanceof Promise` matches the
    promise member (`View | Promise<View>`), when no class named `Promise` is in scope.
  - `x == literal` / `x != literal` selects the literal's member.
  - `"key" in x` keeps the members that may have the key (declare it, optionally or not) and
    `!("key" in x)` those that may lack it ([the `in` operator](#the-in-operator)).
  - Conditions of `if`, `while`, `&&`, `||`, `!`, ternaries and early exits narrow a local
    until it is reassigned; `switch` narrows each case ([`switch`](control-flow.md#switch)).
    A local that a closure assigns is not narrowed ([Null](#null-and-undefined)).
- Printing and template literals show the active member's value. A union with a member that
  cannot be printed (a closure) prints once a test has narrowed it to members that can
  (`typeof v !== "function"`). `JSON.stringify` works on
  unions; `JSON.parse` decodes them when the JSON value tells the members apart (discriminated
  unions by their discriminant; see [`velt:json`](../std/json.md)).
- A union of numbers, bools, strings and literals is copied; one holding an object refers to
  it like any other variable.
- A union alias may name itself behind an indirection: as an array's element, an index
  signature's or a `Map`'s value, or in a function type, as JSON-like types do. Elsewhere
  (`type L = number | { next: L }`, a tuple) a value would contain itself, which is an error:
  give such a type a nominal member, such as an interface or a class.

```ts
type Json = string | number | boolean | null | readonly Json[] | { readonly [key: string]: Json };
type Tree = number | Tree[];

function depth(t: Tree): number {
  if (typeof t === "number") {
    return 0;
  }
  let max = 0;
  for (const k of t) {
    max = Math.max(max, depth(k));
  }
  return max + 1;
}

const v: Json = { a: [1, "x", { b: null }] };
console.log(JSON.stringify(v)); // {"a":[1,"x",{"b":null}]}
console.log(depth([1, [2, [3]]])); // 3
```

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
- A discriminated union may name itself behind an indirection
  (`type Tree = { kind: "leaf"; v: number } | { kind: "node"; kids: Tree[] }`; see
  [Union types](#union-types)).
- Payload enums and `match` do not exist; both are errors with a hint to use a discriminated
  union.

## Type predicates

A function whose return type is a type predicate `x is T` (TypeScript's user-defined type
guard) returns a `boolean`; a call of it in a condition (`if`, `while`, `?:`, `&&`, `||`, `!`)
narrows the variable passed as `x`, as a `typeof` or `instanceof` test would: to the union
members that are `T` (or to the subclass `T`), and to the others when the call is false. A
`T | null` variable narrows to `T`.

```ts
function isString(v: string | number): v is string {
  return typeof v === "string";
}

function show(v: string | number): string {
  if (isString(v)) {
    return v.toUpperCase();
  }
  return (v + 1).toString();
}
console.log(show("ab"), show(41)); // AB 42
```

- TypeScript does not check that a predicate's body answers correctly. A call that narrows
  checks the value at run time against what its result says, as `typeof` or `instanceof` would,
  and panics when they disagree (``the type predicate `isString` returned false for a value
  that is `string` ``; exit code 101), where Node would go on with a value that is not of the
  type it was narrowed to. Unlike a thrown error, this panic cannot be caught with
  `try`/`catch`. A predicate that answers correctly behaves as in Node.
- A literal predicate type (`x is "a"` on `string | number`) narrows to the member it is a
  value of (`string`) when the call is true and leaves the variable as it is when false; a
  type no member of the variable's type is (`x is Dog` on `string | number`) does not narrow.
- The function must be declared with `function` and not be generic for its calls to narrow; a
  predicate on an arrow or a function type (`(x: unknown) => x is T`) is just a `boolean`
  result. `this is T` is accepted and not used for narrowing yet.
- `asserts x is T` and `asserts x` declare an assertion function, which returns nothing (it
  throws instead of returning `false`). A call of it as a statement narrows the variable passed
  as `x` for the rest of the block, as `if (!isT(x)) throw …;` would: to `T`, or for `asserts
  x` to a truthy value (`string | null` to `string`). As for a predicate, the value is checked
  after the call, and an assertion function that returns for a value that is not `T` makes the
  call panic (``the assertion function `assertDog` returned for a value that is not `Dog` ``;
  exit code 101, which `try`/`catch` cannot catch). The errors the function throws itself are
  ordinary errors.

```ts
function assertIsString(v: string | number): asserts v is string {
  if (typeof v !== "string") throw new Error("not a string");
}

function shout(v: string | number): string {
  assertIsString(v);
  return v.toUpperCase();
}
console.log(shout("hi")); // HI
```

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
  expected type, spreads a value of the expected type (`{ ...x, c: 3 }`; Node would show
  or copy the original's fields, in its order), or tests a value of the expected type with `in`
  for a key the copy left out (`"b" in a`, which Node finds on the original), directly or in generic code it calls (`xs.indexOf(x)` and `xs.includes(x)` compare with `===`). An array
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
- Interface declarations merge as in TypeScript
  ([declaration merging](classes.md#declaration-merging)).

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

### Type queries and `keyof`

`typeof x` in a type is the type of the value `x`, as in TypeScript: a module constant, a
function, a local variable or parameter (in a function body), or a field of one (`typeof
config.port`); `typeof ns.f` names an export of a namespace import. A constant initialized with a
string or boolean literal has that literal type (`const s = "a"` gives `typeof s`, the type
`"a"`). With a number literal it is `number`, where TypeScript gives the literal type (`const N =
4` gives `4`; number literal types are not complete yet, #462), so `/` on it divides as in
TypeScript. A function's type
includes the result its body gives, so `ReturnType<typeof f>` names a result type nobody wrote
down, even in the signature of another function, declared before or after it. A signature that
needs its own type (`function f(x: ReturnType<typeof f>)`, also through other functions) is an
error, as in TypeScript (TS2502). `ReturnType<F>` is the result type of function type `F`, and
`Parameters<F>` its parameter types as a tuple. `typeof f` of a function with a rest, optional or
default parameter is not supported yet (#936).

`keyof T` is the union of an object type's keys as string literal types (`keyof { a: number; b:
string }` is `"a" | "b"`; a key a mapped type made from a number literal stays that number:
`keyof { [K in 1 | 2]: K }` is `1 | 2`), and on a `Record` with literal keys it is the key type.

```ts
const DEFAULTS = { host: "localhost", port: 8080 };
type Settings = typeof DEFAULTS; // { host: string; port: number }

function parse(line: string) {
  const [key, value] = line.split("=");
  return { key, value };
}

function show(entry: ReturnType<typeof parse>): string {
  return `${entry.key} -> ${entry.value}`;
}

function read(s: Settings, k: keyof typeof DEFAULTS): string {
  return `${s[k]}`;
}

const s: Settings = { host: "example.org", port: 443 };
console.log(show(parse("a=1")), read(s, "port")); // a -> 1 443
```

Not supported yet (each is an error): `typeof` of a generic, `async` or generator function
(function types have no type parameters), of a class or enum itself (TypeScript's constructor
and enum object types; the type of a class's instances is its name), of `this` and of a local
function value; `typeof import("./m")`; type arguments after the name (`typeof f<string>`);
`typeof` of a function or a constant without a type annotation in classes and interfaces (their
fields and methods); `keyof` of a class, of a type parameter (#350) and of a string-keyed `Record`
(TypeScript's `string | number`). A local's type is its declared type: TypeScript's `typeof x`
sees a narrowing (`if (x !== null)`) that Velt's doesn't.

### Conditional, mapped and template literal types

TypeScript's type-level forms work as in TypeScript where their type arguments are known: an
alias like `type Getters<T> = …` is evaluated at each use (`Getters<User>`), at compile time, and
the result is an ordinary object type, union or literal type.

- **Conditional types** `C extends E ? T : F`: `C` is tested for assignability to `E`
  (structurally for object types). When `C` is a type parameter written alone, the test is made
  for each member of a union (`boolean` is `true | false`; `never` gives `never`);
  `[C] extends [E]` tests the whole type. As with `--strict`, function parameters are compared
  contravariantly, an optional field does not fill a required one, an object type whose fields
  are all optional takes only an object type with none of its own or one field in common (`{ b: 1 }
  extends { a?: 1 }` is false), `{}` takes any value but `null` and `undefined`, and number
  literals compare by value (`1` and `1.0` are one type). Two `Record`s relate by their type
  arguments (the key the other way round), else by their fields: `Record<"a" | "b", 1>` extends
  `Record<string, number>`.
  `infer X` in `E` names the part of `C` it stands for: in a generic type, an object type's field,
  a function type, a tuple, or a template literal pattern. An `infer` that finds nothing is
  TypeScript's `unknown`, which Velt supports only as `NonNullable<X>` (`{}`). An `infer X` in
  several function parameters finds their intersection.
- **Mapped types** `{ readonly [K in S as N]?: V }`: one field per key of the literal union `S`.
  Over `keyof X`, each field keeps `X`'s `?` and `readonly` unless `+`, `-` or a modifier says
  otherwise; `-?` also removes `undefined` from the field's type. `as N` renames a key, and a key
  renamed to `never` is dropped; keys renamed alike have the union of the values. A number literal
  key stays a number. With `keyof T` for a type parameter `T`, a union argument gives the union of
  each member's mapped type. Inside these forms, `T[K]` of an optional field is
  `F | undefined`, as TypeScript reads it.
- **Template literal types** `` `on${K}` ``: the union of the strings they spell, with
  `Uppercase`, `Lowercase`, `Capitalize` and `Uncapitalize`; after `extends` they are patterns
  (`` K extends `model:${string}` ``, `` S extends `${infer F}${infer R}` ``). A `boolean` hole
  is `"true" | "false"`, and a hole of literal types each of them spelled out
  (`` `${boolean}x` `` is `"truex" | "falsex"`).
- `NonNullable<T>`, `Exclude<T, U>` and `Extract<T, U>`; `string & K` with `K` a string literal
  type is `K`; `keyof` of a union is the keys its members share.

```ts
type Getters<T> = { [K in keyof T as `get${Capitalize<string & K>}`]: () => T[K] };
type OptionalKeys<T> = { [K in keyof T]: undefined extends T[K] ? K : never }[keyof T];
type Unbox<T> = T extends { value: infer V } ? V : never;

type User = { name: string; age?: number };
const getters: Getters<User> = { getName: () => "Ann", getAge: () => 36 };
const key: OptionalKeys<User> = "age";
const n: Unbox<{ value: number }> = 42;
console.log(getters.getName(), key, n); // Ann age 42
```

**In generic code.** An alias whose body needs an argument that is still a type parameter
(`function f<T>(x: T): Getters<T>`, a generic class's or interface's field `props:
PropsAccessor<TProps>`) stays unevaluated, as in TypeScript, and is evaluated once the type
arguments are known: at a call, at `new`, and for every instantiation the program reaches. In the
generic body a value of such a type can be stored, passed and returned as that same type. Reading
a field of it, assigning it to another type, or assigning anything else to it is not supported yet
in the generic body, even where TypeScript allows it (`return x` from `x: T` into
`PropsAccessor<T>`, `const s: string = t` for `t: IsString<T>`, `t.length`): it reports an error.
Where TypeScript rejects such code too (TS2322, TS2339), the error is the same.

```ts
type PropsAccessor<P> = { readonly [K in keyof P]: P[K] };
type IsString<T> = T extends string ? "yes" : "no";

interface SetupContext<P> {
  props: PropsAccessor<P>;
}

function setup<P>(props: PropsAccessor<P>): SetupContext<P> {
  return { props };
}

function tag<T>(x: T, t: IsString<T>): IsString<T> {
  return t;
}

const ctx = setup<{ title: string }>({ title: "hi" });
console.log(ctx.props.title, tag(1, "no")); // hi no
```

Not supported yet (each is an error): these forms written directly (not through an alias) with an
argument that is still a type parameter (`function f<T>(x: T extends string ? 1 : 2)`); reading a
member of a deferred type in the generic body, which TypeScript answers from the type parameter's
constraint (`T[K]` with `K extends keyof T`, a mapped type over `keyof T`); a mapped type over an
array or tuple type, or over `string`; a template literal type with a `string` or `number` hole
outside a pattern.

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
  literal, `o["name"]` is the same as `o.name`. A key whose type is a string literal or a union
  of them works too, as in TypeScript: `const k = "a-b"; o[k]` reads `o["a-b"]`, and `o[k]` with
  `k: "a" | "b"` reads the field `k` holds when it runs, typed as the union of the fields'
  types (`number | string`). Assigning through such a key (`o[k] = v`, `o[k] += 1`, `o[k]++`)
  needs the fields to have one type; `o[k] += v` reads the field before `v` runs, as JavaScript
  does. The key can be any expression of such a type (`o[pick()]`, `o[keys[1]]`, `h.o[h.k]`),
  evaluated once; the object must be a variable, `this` or a field of one. `console.log` quotes
  the names that are not identifiers, as Node does (`{ 'a-b': 1 }`), and `JSON.stringify` writes them as given. Names
  beginning with `#` or `[Symbol.`, `"__proto__"` (it sets the prototype in JavaScript) and
  quoted method names are not supported. Parameter destructuring isn't supported yet, so quoted
  names in it aren't either.
- **Key order** of an object type is JavaScript's: field names that are array indices (`"0"`,
  `"404"`: canonical, up to 2^32 - 2) come first, ascending, then the others in declaration
  order. `console.log`, `JSON.stringify` and `Object.keys` all follow it. So do a `Record`
  and a `JsonValue` object: their array-index keys come first, ascending, then the others in
  insertion order (`JSON.parse` too).
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

- **Methods in object literals**: `{ name(x: T): R { ... } }` is a property holding the function,
  as `{ name: (x: T): R => { ... } }` is; `async name() {}` holds an async function. As in an
  arrow, the parameter types may be left out where the literal's type gives them. `this` in such
  a method is not supported yet (an object literal is plain data). Getters and setters
  (`get x() {}`, `set x(v) {}`) are supported where the literal implements an interface with
  methods ([Object literals implementing interfaces](#object-literals-implementing-interfaces)),
  not yet in a literal of an object type. A generic method (`match<R>(arms: Arms<R>): R { ... }`)
  parses as a property holding a generic arrow, which is not supported yet there (a generic
  arrow must be the value of a `const`, [Functions](functions.md#arrow-functions-and-function-types)). A generator method `*name() { ... }` (`async *name()`) is
  a property holding a [generator function](functions.md#generator-function-expressions), as
  `name: function* () { ... }` is; a generator method with a symbol key only as
  `*[Symbol.iterator]()`
  ([Iterable object literals](#iterable-object-literals)).
- **Method signatures in object types**: `{ size(n: number): number; stop(): void }` declares
  fields of function type, as `{ size: (n: number) => number; stop: () => void }` does, and
  `label?(p: string): string` an optional one (`((p: string) => string) | null` with the field
  left out as `null`; call it as `o.label?.(p)`). A generic method signature
  (`run<T>(f: () => T): T`) declares a method in an interface; in an object type, and as an
  optional one in an interface (`wrap?<A>(a: A): A`), it declares a field of generic function
  type (`<T>(f: () => T) => T`), which is not supported yet and is reported where it is used.
  A call signature alone (`interface F { (x: T): R }`, `type F = { (x: T): R }`) declares the
  function type `(x: T) => R`: a value of it is called as `f(x)`, any function or arrow of that
  type is one, and `typeof f` is `"function"`. With members or other call signatures, an object
  type or interface with call signatures is a function with properties or an overloaded
  function (TypeScript's `((x: T) => R) & { stop(): void }`), which is not supported yet
  (#921); a generic call signature (`<T>(x: T): T`) is a generic function type and a
  construct signature (`new (x: T): C`; as in TypeScript, `new(…)` in an object type is never
  a method, while in a class it is) a construct type, both planned
  ([Functions](functions.md#arrow-functions-and-function-types)). Optional parameters in a
  call signature (`(x?: number): void`) are not supported yet, and a parameter initializer
  there (`(x = 1): void`) is an error, as in TypeScript. An interface with call signatures
  merges with the other declarations of its name, but not yet in `declare global` or
  `declare module`, nor inside a function body.

```ts
interface Formatter {
  (value: number, unit: string): string;
}

const fmt: Formatter = (value, unit) => `${value} ${unit}`;
console.log(fmt(3, "kg"), typeof fmt); // 3 kg function
```

```ts
type Handler = {
  get(target: string, key: string): number;
  label?(prefix: string): string;
};

const h: Handler = {
  get(target, key) {
    return target.length + key.length;
  },
};
console.log(h.get("ab", "c"), h.label?.("x") ?? "none"); // 3 none
```

- **`satisfies`**: `value satisfies T` checks `value` against `T`, with `T` as its context (as
  an annotation `const x: T = value` does), and is `value`: `{ version: "1" } satisfies Stamp`.
  Its type is `T`, as for the annotated `x`; TypeScript keeps the value's own, more precise
  type instead (a field `T` declares as `number | string` but given a number is a `number`
  after `satisfies` there), which Velt does not do yet.
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
  takes the default when the field is absent (optional) or `undefined`, and
  `const [first = 0] = xs;` when the array is too short or the element is `undefined`, as in JS;
  a field or element declared `T | null` keeps its `null`, and so does an optional field of an
  object type declared `a?: T | null` when it is present (for a class field declared that way a
  default is [not supported yet](#null-and-undefined)). Defaults in `for...of` patterns and parameter patterns
  are not supported.
- **Arrays** `T[]`: `length`, `xs[i]` (bounds-checked: panics
  `index out of bounds: the len is L but the index is I`), `push` (any number of items:
  `xs.push(a, b)`, `xs.push(...ys)`, all evaluated before the first is pushed, as in JS),
  `pop(): T | null`,
  `forEach map filter reduce find findIndex some every indexOf lastIndexOf includes slice concat
  reverse isEmpty entries fill`, `join` and `toString()` (any elements, written as
  `${xs}` writes them), `sort()` on
  numbers, strings and `Comparable` elements, and `sort(cmp)` (stable, any element type, like
  JS's `Array.prototype.sort(compareFn)`). Callbacks get the element and its index, like JS
  (`xs.map((x, i) => …)`), and may take fewer parameters. A predicate (`filter`, `find`,
  `findIndex`, `some`, `every`) may return any value, tested for
  [truthiness](variables.md#conditions-truthiness) as in JS:
  `[0, 3, 0, 4].filter((n) => n)` is `[3, 4]`. The full list is in the
  [prelude](../std/prelude.md#arrays). Arrays, strings and maps are
  [`Iterable`](control-flow.md#iterables): they convert to `Iterable<T>` values, and
  `xs[Symbol.iterator]()` returns an `Iterator<T>`.
- `new Array<T>(n).fill(v)` and `Array.from({ length: n }, (_, i) => f(i))` build an array of
  `n` elements in one allocation. A bare `new Array<T>(n)` is an error: arrays have no holes.
  `Array.from(src)` and `Array.from(src, (v, i) => …)` copy (and map) anything `for...of` takes:
  an array, a string's characters, a map's entries, a generator, any iterable.
- **`readonly T[]`** and **`ReadonlyArray<T>`** (the same type) are array types, and
  `readonly [A, B]` a tuple type; a `T[]` is passed where one is expected. A variable or
  parameter declared with a read-only array type cannot be written through: a mutating method
  (``property 'push' does not exist on type 'readonly number[]'``, TypeScript's TS2339, also
  `pop`, `shift`, `unshift`, `splice`, `sort`, `reverse`, `fill`, `copyWithin`) and an element
  assignment (`xs[0] = x`, TS2542) are errors; the elements' own fields may change
  (`xs[0].n = 1`). The check follows the variable: a `readonly T[]` copied into a `T[]`
  variable or passed as a `T[]` parameter (TypeScript's TS4104) is not reported yet.
- **Tuples** `[A, B]`: `t[0]`, destructuring, printed like arrays. Element labels document the
  elements and are otherwise ignored, as in TypeScript: `type Wire = [kind: string, b64:
  string]`. Optional elements (`[A, B?]`, `[name: string, age?: number]`) and rest elements
  (variadic tuple types, `[A, ...T]`, `[first: string, ...rest: number[]]`) parse, with
  TypeScript's errors for their order (an optional element after a rest one, TS1266; a required
  one after an optional one, TS1257), and using one is an error saying it is not supported yet.
  `Promise.all` over tuples of different types is not supported.
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
  ([Map](../std/prelude.md#map)). Iteration follows insertion order, like JS. A value type may
  include `null` (`Map<string, number | null>`, `Record<string, T | null>`): an entry holding
  `null` is present like any other (`has` is true, it is counted, iterated and printed), and
  `get` gives `null` both for it and for a missing key (use `has` to tell them apart). If
  such an entry holding `null` has a content key that changed after insertion, then while the
  map has deleted entries not yet compacted away, `forEach`, `for...of` and `console.log` skip
  it (`keys()`, `values()` and `entries()` list it).
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
  may spread another record (`{ ...r, x: 1 }`) and have computed keys (`{ [k]: v, [a + b]: w }`,
  in order with the other keys); an object literal with a computed key that is not a module
  constant, or one that spreads a variable `r` of type `Record<string, V>`, is a
  `Record<string, V>` also where no record is expected (spreading a record into an object of a
  fixed shape is an error: its keys are known only at run time). A string or integer
  literal key in brackets is the plain key (`{ ["a"]: 1 }` is `{ a: 1 }`), as is a numeric
  property name (`{ 7: v }` and `{ 0x10: v }` have the keys `"7"` and `"16"`). A computed key
  is a `string`, or a number converted to its string as in JavaScript (`{ [i + 0.5]: v }` with
  `i = 3` has the key `"3.5"`); a symbol made at run time is an error, and so is a key of any
  other type (`{ [true]: v }`, TypeScript's TS2464). In generic code, where the key type is a type
  parameter `K`, reads are `V | null` and the record may be closed, so it cannot start empty
  (only a literal with a spread builds one) and `delete` is not allowed. A record has no
  methods of its own and is not iterable: `Object.keys(r)` (a `string[]`), `Object.values(r)`
  and `Object.entries(r)` return arrays in JavaScript's order: keys that are array indices
  (`"0"` to `"4294967294"`) first, ascending, then the others in insertion order (`for (const
  [k, v] of Object.entries(r))`). `console.log` and `JSON.stringify` use the same order. Given an object literal, `Object.values` and `Object.entries` read it
  as a `Record<string, V>`, so its values need one type. `Object.keys` accepts any object, as
  in TypeScript: an object literal or object type (`Object.keys({ a: 1, b: "x" })` is `["a",
  "b"]`), a struct, or a class instance, whose fields it lists in declaration order (base class
  fields first, `private` ones too; not ES private `#x` fields, static fields or methods). A struct's optional field is
  listed only when it is not `null`. On a class with subclasses it lists the fields of the
  object's actual class (a `Shape` holding a `Rect` lists the `Rect` fields too), and on an
  interface value those of the class it holds (an interface also implemented by a struct is an
  error: struct values carry no class). On an `any` it lists the fields of the class instance
  held, when every value the program converts to a dynamic type is a class instance; in a
  program that also converts other values (an array, a string, an object type, a function) it
  is a compile error for now (#924, #926). `console.log` and `JSON` treat a record as an object. A class
  cannot `extends` a `Record` (its constructor would leave a closed record without its keys);
  hold one in a field instead. A literal for an enum-keyed record is not supported yet.
- **Index signatures**: an object type or interface with an index signature and no other
  members, `{ [key: string]: V }`, is `Record<string, V>`, as in TypeScript where the two are
  interchangeable: a value converts between them and stays the same object, and everything
  above holds (any key may be read, written and deleted; keys are listed in JavaScript's
  order, array-index keys such as `"2"` first). A generic
  interface works too (`interface Groups<T> { [name: string]: T[] }`). Reading a key that is not
  there gives `null` where JavaScript gives `undefined` (#930), so a read is `V | null`
  (TypeScript types it `V`): `d.k === null` is `true` and `typeof d.k` is `"object"` for a
  missing key, `d.k === undefined` is a compile error, and `d[k] ?? 0`, `d[k] || 0` and
  `d[k]?.push(x)` work as in JavaScript. The value type may include `null` (`{ [k: string]:
  number | null }`): a key holding `null` is present (listed by `Object.keys`, written by
  `JSON.stringify`), and reading it gives `null` as for a missing key. As for `readonly T[]`, a `readonly`
  index signature is not checked for writes yet. Not supported yet (each is an error): named
  members next to an index signature (`{ name?: string; [key: string]: string | undefined }`,
  which needs `undefined`, #930), a `number` or `symbol` key, two index signatures in one type,
  and an interface with an index signature that also has methods or `extends`. An object of a
  named object type does not convert to an index-signature type (TypeScript allows it for type
  aliases but not interfaces): build a record (`{ ...o }` with values of one type) instead.

```ts
interface Scores {
  [player: string]: number;
}

const s: Scores = { bob: 2 };
s.alice = 3;
s["carol"] = (s["carol"] ?? 0) + 1;
const r: Record<string, number> = s; // the same object
console.log(Object.keys(r), s.dave ?? "none"); // [ 'bob', 'alice', 'carol' ] none
```
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

### The `in` operator

`"key" in o` tests whether the object `o` has the member `key`, as in JavaScript. Velt objects
have fixed fields, so a literal key is answered from `o`'s type while compiling: a field, method
or accessor its object type, class (or a base class) or interface declares is always there, and
one it doesn't declare is not. A setter makes its name a member (`"w" in o` for `set w(v)`);
static members are the class's, not its instances', and a `#private` name is never a string key
(`"#p" in o` is `false`). An optional field of an object type or interface (`b?: T`) is tested
at run time: it is there once the object has it, also holding `null`, where the type keeps that
apart. A class's optional field is always there (TypeScript-compiled classes define it). A value
whose class is known only at run time is tested for it: a `Base` holding a `Sub` has the members
`Sub` declares, and an interface value has those of the class it holds, an optional field the
interface declares included. The members every object inherits from `Object.prototype`
(`"toString"`, `"constructor"`, `"hasOwnProperty"`, …) are there on every object, as in Node. A
`Record<string, V>` is a dictionary, so its keys are tested at run time, also a key known only
then (`k in r`): a key `delete` removed is gone, and a number key is its string (`1 in r` tests
`"1"`). Such a key may have any type that is a string or a number: `string`, a union of string
literals (`"a" | "b"`, a `keyof` type), `string | number`, or an enum (a string enum's key is its
value, a numeric enum's its value as a string). A closed record
(`Record<"x" | "y", V>`) has exactly its keys. On a union, `in` answers for the member the value
holds, and in a condition it narrows the union ([union types](#union-types)).

A key known only at run time on anything but a `Record<string, V>` is an error (``` `in` with a
key known only at run time is not supported on `T` ```): there is no table of names to look in.
So is a key Velt cannot answer while compiling: a member declared by a struct an interface value
may hold (structs carry no class to tell them apart), and a string key on a built-in or standard
library value (an array, a `Map`). A value converted to a narrower object type by copying its
fields (`const p: P = full`, see [intersection types](#intersection-types)) is an error when the program
tests it for a key the copy left out, since Node would find it on the original. An optional
field (`b?: T`) is its value or nothing, while Node also finds a key that holds `undefined`
(`"b" in { b: undefined }` is `true`). So a program that tests such a field with `in` and
somewhere stores a value that may be `undefined` in a field of that name (`{ kind, b }` with an
optional parameter `b`, `o.b = maybe`, `{ b: other.b }`, a spread of a `b: T | null` field) is
an error (``` cannot tell an absent `b` from one holding `undefined` ```, which points at the
first such store and says how many others there are): store the value only
when there is one (`if (v != null) o.b = v`), or test the value (`o.b != null`). Leaving the
field out, a value that cannot be null and a spread of an object with the same optional field
are fine. A primitive
right operand is an error (TypeScript's TS2322; JavaScript throws a `TypeError`), as is a
possibly `null` one (TS18047), and so is a left operand that is not a string, a number or a
symbol (TS2322). Symbol keys are described under [Symbols](#symbols), and `#x in o` under
[private names](classes.md).

```ts
type Circle = { kind: "circle"; radius: number };
type Square = { kind: "square"; side: number; label?: string };

function area(s: Circle | Square): number {
  return "radius" in s ? 3 * s.radius * s.radius : s.side * s.side;
}

class Base {
  x = 1;
}
class Sub extends Base {
  y = 2;
}

function main() {
  const sq: Square = { kind: "square", side: 2 };
  console.log(area(sq), "label" in sq, "toString" in sq); // 4 false true
  const seen: Record<string, number> = { a: 1 };
  const k: string = "a";
  delete seen[k];
  console.log(k in seen); // false
  const b: Base = new Sub();
  console.log("y" in b, 1 in seen); // true false
}
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
- Object literals are plain data in Velt, so this method holds the iterable's whole state. The
  literal can have no other members, and `this` in the method is an error (in TS it is the
  object): use variables, or declare a class that `implements Iterable<T>` and reads its
  fields. Other generator methods in object literals are errors too: write a property holding a
  generator function.
- The value is an instance of the prelude class `__IterableObject<T, E>` (async:
  `__AsyncIterableObject<T, E>`), which holds the method; annotate it as `Iterable<T>`.

### Object literals implementing interfaces

Where an interface with methods is expected (an annotation, a parameter, a return type or
`satisfies`), an object literal implements it, as a class that declares `implements` does:

- A method is a function value: `m(x) { … }`, `m: (x) => …` or `async m(x) { … }`. Its
  parameter types may be left out: they come from the interface's method.
- A field is a value, or a **getter** `get f() { … }`, which runs on each read. A getter
  method of the interface (`get f(): T`) takes a getter or a value (`f: 5`), and a setter method
  (`set f(v: T)`) a setter `set f(v) { … }`, which runs on each `x.f = v`. A field of the
  interface can't be written through the interface value yet (`x.f = v` is an error), so a
  setter given for a field never runs: declare a setter method in the interface instead.
- A method with a default body and an optional field may be left out (the field is `null`);
  every other member must be given, and a member the interface doesn't declare is an error.
- The value is an interface value: its members are read and called through the interface.
- Such a literal may be a module constant, exported or not (`export const upper: Handler = {
  … }`). Like every module constant it is evaluated at each use, so comparing it by identity
  (`===`) is an error, and each use passed on is a new value, as for a function-valued constant
  ([Variables](variables.md#module-constants-initialized-by-a-call)).

```ts
interface Counter {
  next(step: number): number;
  readonly count: number;
}

function counter(): Counter {
  let n = 0;
  return {
    next(step) {
      n += step;
      return n;
    },
    get count() {
      return n;
    },
  };
}

const c = counter();
c.next(2);
c.next(3);
console.log(c.count); // 5
```

- The methods use the variables around them, as arrows do; `this` in them is not supported yet.
- A generic interface method (`map<R>(f: (x: T) => R): R`) cannot be a function value: declare a
  class that implements the interface.
- A method that throws must be allowed to by the interface method's function type, as for any
  function value: an optional method (`m?(x: T): R`) is a field of function type, which
  doesn't allow throwing unless it says `throws`.
- `console.log` and `JSON.stringify` of such a value are not supported yet (as for other
  interface values).

## any and unknown

`any` and `unknown` hold a value of any type together with its type, like C#'s `dynamic`. Every
value converts to either. An `any` converts back to every type, and the value is checked where it
enters typed code: a value of another type stops the program with a `TypeError` (Node would
keep the wrong value). An `unknown` must be narrowed first (`typeof`, `instanceof`), or
converted with `as T`, which is checked the same way. Both are the same value at run time.

```ts
class Point {
  x: number;
  constructor(x: number) {
    this.x = x;
  }
}

function main(): void {
  const p = new Point(1);
  const a: any = p;
  const q: Point = a; // checked: `a` holds a `Point`
  console.log(q === p, typeof a); // true object
  const n: any = null;
  console.log(n == undefined, n === undefined); // true false
  const u: unknown = "text";
  if (typeof u === "string") {
    console.log(u.length); // 4
  }
}
```

What works on `any` and `unknown` today: conversions in and out (also `null`, `undefined`,
`T | null`, unions and class instances, checked against the class and its subclasses, and an
instance of a generic class against that very instantiation (`Box<number>`, so a
`Record<string, number>` converts only from a record of that type: an object of a fixed shape
stops with a `TypeError` for now, #1219; Node reads any object as a record); into a
union or a literal type such as `"red" | "green"` the value takes the first member it is: a
`typeof` test for `number`, `string` and `boolean`, `instanceof` for a class, `===` for a
literal), `typeof` (narrowing to `number`, `string` and `boolean`), `instanceof`, truthiness,
`===`, `!==`, `==` and `!=` (JavaScript's loose equality: `null == undefined`, `"1" == 1`) on any
operand whose type is `any` or `unknown` (a variable, a field, an element, a call), and
`console.log`. An object is itself in an `any` whatever its static type was when it was
converted (a class instance through its base class, an interface value, a function converted
twice). As in TypeScript, a union with `any` is `any` (`any | null`, `any | undefined`) and one
with `unknown` is `unknown`: the `null` or `undefined` is held by the value itself, so
`m.get(k)` of a `Map<string, any>`, `r[k]` of a `Record<string, any>` and `find` on an `any[]`
give an `any` that is `undefined` when nothing is there, and `null` when a `null` was stored;
a left-out optional field `q?: any` holds `undefined`. An enum value is its number or string
in an `any` (`typeof` is `"number"` or `"string"`), and converts back to the member with that
value. Generic code compares an `any` as `===` does (`indexOf`, `includes`, a generic
`a === b`), and `Set<any>` and `Map<any, V>` keys compare by value as in JavaScript (`NaN` is
itself, `-0` is `0`), objects by identity.
Differences from Node: `null` and `undefined` converted to `T | null` both give `null`
(a `T | null` has no `undefined`; convert to `T | undefined` to keep it); `==` between an
object and a boolean, number or string stops with a `TypeError` (JavaScript converts the object
with `ToPrimitive`: `[] == 0` is `true`); an instance of a subclass of a generic class does not
convert to that class (`Sub extends Box<number>` to `Box<number>`: a `TypeError`); a left-out
optional parameter `x?: any` holds `null`, not `undefined` (as with every optional parameter
today); and a function prints as `[Function (anonymous)]` (Node prints its name, `[Function: f]`).

Members of an `any` are read and called as in JavaScript: `x.f`, `x["f"]`, `x[k]` with a
`string`, `number` or `any` key (any expression: `x[key()]`, `x[i + 1]`), `x.m(a, b)` (a method,
or a field holding a function, typed or `any`) and `x(a, b)` when `x` holds a function. The member
is found at run time in the value the `any` holds: a field, getter or method of a class
instance (a subclass's too when it was converted through its base class), an object type's
field, `length` and elements of an array, `length` of a string, and the members of the standard
library's classes (`size`, `get`, `set` and `has` of a `Map`). The result is an `any`; each
argument is converted to the parameter's type with the same check as above, a left-out one is
`undefined`, and extra ones are ignored.

```ts
class Counter {
  n: number = 0;
  get double(): number {
    return this.n * 2;
  }
  add(k: number): number {
    this.n += k;
    return this.n;
  }
}

function main(): void {
  const c: any = new Counter();
  console.log(c.add(5), c.double, c.missing); // 5 10 undefined
  const m: any = new Map<string, number>();
  m.set("a", 1);
  console.log(m.get("a"), m.size); // 1 1
  const xs: any = [10, 20];
  console.log(xs.length, xs[1], xs[2]); // 2 20 undefined
  const f: any = (s: string) => s.toUpperCase();
  console.log(f("hi")); // HI
}
```

A member the value's fixed shape does not have reads as `undefined`, as in Node. A `null` or
`undefined` receiver stops with Node's `TypeError: Cannot read properties of undefined (reading
'f')` before the call's arguments are evaluated, calling something that is not a function with
`TypeError: x.f is not a function`, and an argument of the wrong type with the conversion's
`TypeError`. Optional chaining short-circuits as in Node: `x?.f`, `x?.m()`, `x?.[0]` and
`x.f?.()` are `undefined` when the link is `null` or `undefined`, and the rest of the chain
(`x?.a.b`) is not evaluated. What this phase cannot reach yet
stops with a `TypeError` naming #924 rather than giving a wrong value: a method read without a
call, methods of arrays and strings, string indexing, methods that throw or are generic, and
members whose type is a union, enum or interface.

Members of an `any` are written as in JavaScript too: `x.f = v`, `x["f"] = v` and `x[k] = v`
with a `string`, `number` or `any` key. The value is converted to the member's type with the
same check as above (a value of another type stops with its `TypeError`, and the member keeps
its old value) and stored in the object itself, so every other reference to it sees the write;
a setter is called with it. A `Record<string, V>` takes any key, adding the ones it lacks, as
in Node; an array takes its elements inside its length. The other values have a fixed shape:
adding a property stops with `TypeError: Cannot add property k: an object of a fixed shape gets
no new properties through `any` (#924)` (Node adds it), writing a getter that has no setter with
Node's `TypeError: Cannot set property k of #<C> which has only a getter` (what strict code
gets; in sloppy code Node ignores the write), and a `null` or `undefined` receiver with Node's
`TypeError: Cannot set properties of undefined (setting 'k')`, after the value is evaluated.
Writing a `readonly` field, a method, an element past the end of an array or a member of a
string, number or function stops with a `TypeError` naming #924. Compound assignments
(`x.n += 1`, `x.n ??= v`) and `x.n++` are compile errors until the operators of `any` come.

`Array.isArray(x)`, `x instanceof C` for a generic class `C` (TypeScript's `C<any>`, including
`Map` and `Set`) and `typeof x === "function"` narrow an `unknown` to an `any` (and leave an
`any` one): the value stays as it is, and its members are read, called and written as above.
`typeof` tests for `number`, `string` and `boolean` and `instanceof` of a class without type
parameters narrow to that type itself, so member access after them is typed and direct.

```ts
class Box<T> {
  v: T;
  constructor(v: T) {
    this.v = v;
  }
}

function describe(u: unknown): string {
  if (Array.isArray(u)) {
    return `${u.length} items, first ${u[0]}`;
  }
  if (u instanceof Box) {
    return `a box of ${u.v}`;
  }
  return "something else";
}

function main(): void {
  console.log(describe([3, 4])); // 2 items, first 3
  console.log(describe(new Box("s"))); // a box of s
  const p: any = { x: 1, y: 2 };
  p.x = 10;
  p["y"] = 20;
  console.log(p.x, p.y); // 10 20
  const r: any = { a: 1 } as Record<string, number>;
  r.b = 2;
  console.log(r.b); // 2
}
```

**Planned** (#924): compound assignments and `++` on members, arithmetic and comparison
operators, `String(x)`, `JSON.stringify`, iteration, `any` converted to interface types and the
warning on a written `any`; until then each is a compile error.

`key in x` on an `any` asks the value held: a class instance or object type has its fields,
methods and accessors and what every object has from `Object.prototype` (`toString`), a
`Record<string, V>` its keys, an array its `length`. A `null` or `undefined` value stops with
Node's `TypeError: Cannot use 'in' operator to search for 'k' in undefined`; an optional field
(which may be absent) and an array index stop with a `TypeError` naming #823 for now.

### Generic interfaces with `unknown` arguments

A value of a generic interface type with concrete type arguments converts to the same interface
with `unknown` (or `any`) arguments: a `Handler<Date, number>` is a `Handler<unknown, unknown>`,
so a table of handlers written for different types can be one `Handler[]`. TypeScript accepts
this because it compares method parameters bivariantly (an interface's type parameter may be a
method parameter's type, `serialize(value: T)`), which lets a wrong argument through unnoticed.
Velt checks it instead: the converted value wraps the original, and every call through it
converts each argument to the original's own type, checked at run time as a conversion out of
`any` is (a value of another type stops the program with a `TypeError`; Node would run the
method with it). Results and fields need no check (an `unknown` takes any value), and a function
value in a parameter, a result or a field (an optional method `revive?(e: E): T` is a field of
function type) is wrapped the same way, as is an interface value of a result.

```ts
interface Handler<T = unknown, E = unknown> {
  name: string;
  test(value: unknown): boolean;
  serialize(value: T): E;
}

const dateHandler: Handler<Date, number> = {
  name: "date",
  test: (v) => v instanceof Date,
  serialize: (d) => d.getTime(),
};

function encode(value: unknown, handlers: Handler[]): void {
  for (const h of handlers) {
    if (h.test(value)) {
      console.log(h.name, h.serialize(value)); // date 0
    }
  }
}

function main(): void {
  encode(new Date(0), [dateHandler]);
}
```

Calling `serialize("x")` through the `Handler[]` stops with `TypeError: the value is not a Date
(it is a string)`. The wrapped value is the original object: a write through one is seen
through the other, and every copy of the converted value (an array element read into a
`const`, a second conversion to `Handler<any, any>`) wraps that same object.

A parameter of type `unknown[]`, `Map<unknown, unknown>` or `Set<unknown>` (a handler for
arrays, maps or sets, as in a serializer's table) takes an array, map or set of any element
types. The method receives a copy holding the elements boxed, and when it returns the copy is
written back into the caller's array, map or set. Each element written back converts to the
original's own element type, checked as above. Reads and changes the method makes during the
call match Node: it sees every element, and the caller sees what it added, deleted or replaced.
Differences from Node, tracked in #1185:

- the method's container is not the caller's: `===` between the two is `false`, and a
  reference the method keeps after it returns no longer follows the caller's container;
- changing the length of an `unknown[]` (`push`, `pop`) stops with a `TypeError` naming #1185.

```ts
interface Handler<T = unknown, E = unknown> {
  name: string;
  serialize(value: T): E;
}

const mapHandler: Handler<Map<unknown, unknown>, [unknown, unknown][]> = {
  name: "map",
  serialize: (m) => [...m.entries()],
};

function main(): void {
  const all: Handler[] = [mapHandler];
  const scores = new Map<string, number>([["ann", 3]]);
  console.log(all[0].serialize(scores)); // [ [ 'ann', 3 ] ]
}
```

The conversion applies where each type argument is the original's or `unknown` / `any`, and
each parameter and result converts as described. It is a compile error, with a note naming the
case, when a method's parameter has another type holding `unknown` or `any` (a generic class
such as `Box<unknown>`, or `Map<string, unknown>`; #1185) or is an interface (#924), and when a
result is a promise of another type, a parameter is an array of the type parameter, or a method
has type parameters of its own. A parameter of a function value (an optional method
`revive?(e: E): T` with `E` an `unknown[]`) is checked against its own type as it is (#1185),
except a generic class with `unknown` arguments, which is a compile error. A program without
such a conversion is unchanged.

### Proxy and Reflect

`new Proxy(target, handler)` makes a proxy as in JavaScript: reading a member of it runs the
handler's `get` trap, writing one its `set` trap and `key in p` its `has` trap, each with the
target, the key (a string; an index arrives as its string, `"1"`) and, for `get` and `set`, the
proxy itself as the receiver. A trap the handler leaves out forwards the operation to the
target, and `p.m(a)` reads `m` through `get` and calls what it returns. A `set` trap that
returns `false` makes the assignment throw Node's `TypeError: 'set' on proxy: trap returned
falsish for property 'k'` (what strict code gets). `Reflect.get(t, k)`, `Reflect.set(t, k, v)`
and `Reflect.has(t, k)` do what `t[k]`, `t[k] = v` and `k in t` do; given a receiver, `Reflect.get`
and `Reflect.set` pass it on to a target that is itself a proxy. `console.log(p)` prints the
target, without running traps, as Node does.

The handler is a `ProxyHandler<T>`: its traps' parameters take their types from it, as in
TypeScript (`target: any`, `p: string | symbol`, `receiver: any`). `new Proxy(target, handler)`
is an `any`, which converts to the class or object type the context expects (`as State`, an
annotation, a return type, an argument). The typed value is the same proxy: reading `s.count`,
writing `s.count = 1`, `s.count += 1` and passing it on run the traps as before, `s === t` and
converting it back to `any` see one object, and `console.log(s)` prints the target. A field
read through the `get` trap is checked against the field's type when it arrives (a `TypeError`
on a mismatch), so a proxy may stand for a type whose fields its target doesn't have (an empty
target whose `get` trap answers every key). A method called through a typed proxy runs with
`this` the proxy, so its reads and writes of `this` run the traps too. `s.count++` used as a
value runs the `get` trap once, and `(s.count = v)` used as a value is `v` without a `get`, as
in JavaScript. A field read through a typed proxy runs after the operands before it, as any
call would (`console.log(n, s.count, n)` reads the first `n` before the trap runs).

`s instanceof C` answers for the proxy's target, as JavaScript does when the handler has no
`getPrototypeOf` trap. A typed proxy as a `WeakMap` or `WeakSet` key, or a `WeakRef` target, is
the proxy itself, not its target, and `deref()` gives the proxy back. An entry that refers back
to its key through a typed proxy (a `raw -> proxy` cache, or a value holding its proxy key) goes
away with the key, as other such entries do. A typed proxy that goes to
another task (`spawn`, a `shared` value or `Mutex`, a channel) is copied like any other value
there: the proxy object together with its target and handler, so it is still a proxy on that
task, and changes there don't reach the original.

Only the types a proxy is converted to (and the classes related to them) check for one, and only
in a program that creates a proxy, so other programs and other types pay nothing.

```ts
let reads = 0;

function counted(target: any): any {
  return new Proxy(target, {
    get(obj, prop, receiver) {
      reads++;
      return Reflect.get(obj, prop);
    },
    set(obj, prop, value, receiver) {
      console.log("set", prop, value);
      return Reflect.set(obj, prop, value);
    },
  });
}

function main(): void {
  const raw = { count: 0 };
  const state = counted(raw);
  state.count = 5; // set count 5
  console.log(state.count, raw.count, reads); // 5 5 1
  console.log("count" in state, "other" in state); // true false
}
```

**Planned** (#823): the `deleteProperty`, `ownKeys`, `getOwnPropertyDescriptor` and
`defineProperty` traps and their `Reflect` functions; symbol keys through a proxy; proxies typed
as an array or a `Record`, and `in` on a typed proxy; a typed proxy passed where an object type
with other fields is expected (it is copied there); a function the `get` trap returns, called
as a method of a typed proxy; `Array.isArray` of an array proxy; proxies of functions (`apply`,
`construct`); a method call through a proxy whose handler has a `get` trap (`p.m()`, where
JavaScript reads `m` through the trap first); `Reflect.get` / `Reflect.set` with a receiver
that is neither the target nor a proxy; destructuring a typed proxy
(`const { n, label } = s`) and `JSON.stringify` of one. Until then each is a compile error or a `TypeError`
naming #823. A method call through an `any` proxy whose handler has no traps is the call on
the target. When the receiver is a proxy, a getter of a plain target read by
`Reflect.get(t, k, receiver)` runs with `this` the target, and so does a setter written by
`Reflect.set(t, k, v, receiver)` (Node: the receiver).

## object and Function

`object` holds any value that is not a primitive: a class instance, an object, an array, a map,
a set or a function, but not a `number`, `string`, `boolean`, `symbol`, `null` or `undefined`.
`Function` holds any function. Both are the dynamic value of `any` restricted to those values,
with TypeScript's rules:

- Only objects convert to `object`, and only functions to `Function` (a `Point | null` does not
  convert to `object`: test it for `null` first). Both convert to `any` and `unknown`, a
  `Function` to `object`, and an `any` to either.
- `T extends object` takes only types whose values are objects, so overloads can tell objects
  from primitives; inside, a `T` converts to `object`.
- An `object` converts back with `as T`, checked like an `any` (a `TypeError` when it holds
  something else). Its members are TypeScript errors until it is narrowed or converted.
- Calling a `Function` is a call through `any`: the arguments are converted to the parameters'
  types when the function runs, and the result is an `any`. A field of type `Function` is
  called the same way (`plugin.setup(app)`).
- `Set<object>` and `Map<object, V>` (and `Map<Function, V>`) key their entries by identity, as
  in JavaScript.
- An `any` or `unknown` converts to `object` with `as object` (an `any` also without it),
  checked at run time: a `boolean`, `number`, `string` or `symbol` is a `TypeError` (Node would
  keep the primitive); `null` and `undefined` pass.
- A value of a type parameter without an `object` bound converts with `as object` too
  (`function toRaw<T>(observed: T) { ... observed as object ... }`): a `T | null` or a union
  is unpacked to the value it holds, and a primitive is the same `TypeError`.
- `object` can be a `WeakMap` key, a `WeakSet` member and a `WeakRef` target
  ([WeakMap, WeakSet and WeakRef](../std/prelude.md#weakmap-weakset-and-weakref)): the key is
  the object it holds, whichever type it reached `object` through. A primitive (passed as an
  `any`), `null` or `undefined` finds nothing in `get`, `has` and `delete`, and is
  JavaScript's `TypeError` in `set`, `add` and `new WeakRef`; a value Velt cannot hold weakly
  yet (a function value, a tuple) is a `TypeError` there too. A variable passed as a key or
  target stays alive to the end of its scope. `Function` keys are not supported yet.
- `typeof u === "object"` narrows an `unknown` to `object` and `typeof u === "function"` to
  `Function`; a union with `object` or
  `Function` is that type when its other members are objects (or functions), `null` and
  `undefined` included, and `unknown` otherwise (`string | Function`). `f?.(x)` on a
  `Function | undefined` (an optional `onClick?: Function` field) skips the call when it holds
  `null` or `undefined`, as in JavaScript.

```ts
class Point {
  x: number;
  constructor(x: number) {
    this.x = x;
  }
}

function brand<T extends object>(v: T, seen: Set<object>): T {
  seen.add(v);
  return v;
}

function main(): void {
  const seen = new Set<object>();
  const p = brand(new Point(1), seen);
  const o = brand({ name: "a" }, seen);
  console.log(seen.has(p), seen.has(o), seen.has(new Point(1))); // true true false
  const handlers: Record<string, Function> = {};
  handlers["double"] = (n: number): number => n * 2;
  console.log(handlers["double"](21)); // 42
  const back = [...seen][0] as Point;
  console.log(back.x); // 1
}
```

**Not yet** (#926): `object` as a `WeakMap` key, a `WeakSet` member or a `WeakRef` target, the
members every object has (`constructor`, `toString`, `hasOwnProperty`) and a function has
(`call`, `apply`, `bind`, `name`, `length`), `Object.keys` of an `object`, and a type parameter
without the `object` bound converted with `as object`. Each is a compile error.
