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
| `bigint` | no values yet: `never` until Velt has `BigInt` values, so `string \| bigint` is `string` and `T extends bigint ? A : B` is `B` ([bigint](#bigint)) |
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
  An assertion that narrows, `x as T` with `x: T | null` (or `| undefined`), `u as A` with
  `A` a member of union `u`, and `u as A | B` (also `| undefined`) with some of `u`'s members,
  is checked like `x!`: TypeScript trusts it, Velt panics with `type assertion failed: …` when
  the value is not a `T`; `u as A | B` of a `u: A | B | C | undefined` panics on a nothing
  as `u!` does. In generic code, `x as T` of an `x: T | undefined` is checked only where `T`
  has no `undefined` of its own: at `T = U | undefined` every value of `x` is a `T` and passes
  as it is. At `T = U | null` an `undefined` is no `T` value (Velt's `U | null` cannot hold
  one, #930), so it stops with a `TypeError` instead of reading as `null`: an assertion never
  changes the value. `u as A` with `A` a member of `u` is checked in generic code too
  (`mod as Mod<T>` with `mod: T | Mod<T>`), and so is a union of some of `u`'s members that are
  not themselves type parameters (`r as Ok<T> | Err` with `r: Ok<T> | Err | Pend`). Where a
  member is a type parameter, the check follows the instantiation: at `T = unknown` (or
  `any`) every value is a `T`, so `x as T` of an `x: T | string` holding a string passes, and
  `mod as Mod<T>` of a `T` converts that `unknown` as `u as Mod<unknown>` would (#924). A
  tuple asserts element by element
  (`t as [object, string]` with `t: [any, string | symbol]`), each element converted or checked
  as it would be alone; the result is a new tuple of those elements, so a write into one of the
  two tuples does not show in the other, and `t2 === t` is `false` (an element that is an object
  is the same object in both).
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
  `1e+21`, `NaN`, `Infinity`, `0` for `-0`). An array or a tuple is written as `join()` writes
  it (`"" + [1, 2]` is `"1,2"`), a class instance by its `valueOf()` when that returns a
  primitive (`+` asks for it before `toString()`, as in JS; a `Date` by its `toString()`), else
  by its `toString()`, and a value whose type is a type parameter as `String(x)` writes the
  type it is instantiated with (an `any` and a class instance with `+`'s order: `valueOf()`
  first). Any other value
  next to a string is a compile error; build that text with a template literal
  (`` `Total: ${m}` ``).
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
initialized with `Symbol("...")` (the description a string literal or a string constant) or
`Symbol.for("...")` (TypeScript's `unique symbol`;
`const KEY: unique symbol = Symbol("k")` declares the same), or with another such constant, and
may be imported. Velt objects have fixed fields, so the compiler resolves these members while
compiling: reading `o[KEY]` costs what reading a named field does. A symbol key known only at
run time (a symbol made in a function) is an error: use a `Map`. In an object literal, a string
key known only at run time (`{ [o.tag]: v }`, `{ [name]: v }` with a local `name`) makes the
object a `Record<string, V>`, as TypeScript gives it a string index signature. A module constant
holding a string literal names that field the same way (`const NAME = "name"`, `[NAME]: T`).
In a declaration (an interface, an object type or a class), a string or number literal in
brackets is the plain name (`["k"]: number` is `k: number`; `[1]: string` and `1: string` name
the member `"1"`, `[-1]: string` the member `"-1"`, `1e3: number` the member `"1000"`), also
after a modifier (`readonly 7 = 4`), and so in a destructuring pattern (`const { 1.5: a, [-1]: b
} = o`). `[-0]` names the member `"0"`, as `String(-0)` is in JavaScript (TypeScript's checker
reads it as `"-0"`). Such a name is for fields: a method or accessor named by a literal
(`["a-b"]() {}`, `get "c d"()`, `get 1()`, `1e3() {}`) is not supported yet. A symbol made in
place (`[Symbol("x")]: v`, also in an object literal) is an error that asks for a module
constant. An enum member names the member its value spells, as in TypeScript:
`[ReactiveFlags.SKIP]?: boolean` with `SKIP = "__v_skip"` is the member `__v_skip`, and a
numeric enum's member is its number (`[Slot.First]` is `"0"`). It works in declarations, in
object literals (`{ [ReactiveFlags.SKIP]: true }`), in `o[ReactiveFlags.SKIP]` (also as an
assignment target) and in `ReactiveFlags.SKIP in o`, also with an imported enum. In a
destructuring pattern, a computed key is a string or number literal
or a name (`{ [KEY]: v } = o`); a path or another expression (`{ [t.tag]: v } = o`) is an error:
store the key in a variable first (`const k = t.tag; const { [k]: v } = o`).
A class or interface may declare a method, getter or setter with such a key (`[KEY]() {}`,
`get [KEY]()`) or a well-known symbol key (`get [Symbol.toStringTag]()`); `o[KEY]`, `o[KEY]()`,
`o[KEY] = v` and `KEY in o` find it. A quoted key is an ordinary string key, also one that reads
like a symbol (`"[x]"`, `"[Symbol(k)]"`); only names starting with `"[Symbol.` or with the
noncharacter U+FFFF are reserved (Velt names symbol-keyed members with them). `console.log`
shows a symbol-keyed member after the others as `[Symbol(description)]: v`, and
`JSON.stringify` and `Object.keys` leave it out, as in Node; `JSON.parse` never fills one,
whatever keys the text has (a required symbol-keyed field makes the decode fail).

A literal asserted to a type may carry a symbol-keyed member the type doesn't declare
(`{ type, props, [VNODE]: true } as VNode`): TypeScript allows it and the object keeps the
member, so Velt adds the optional field `[VNODE]?: boolean` to `VNode` (an interface or an
object type alias of the program; the field's type is the literal value's, `any` for another
value or for literals of different types). Objects built without it don't have it, as in Node:
`console.log` shows it only where it is set, spreads copy it, `VNODE in v` tests it, and it is
read and written through `any` (`(v as any)[VNODE] === true`, `(v as any)[VNODE] = true` on an
object of a type that has the field). The key is resolved in the module of the literal, so the
type may be declared where `VNODE` is not imported. Reading such a key through `any` from a
value without fields (a string, number, boolean, array or function) gives `undefined`, as in
Node.

```ts
const BRAND: unique symbol = Symbol("brand");

type Tagged = { tag: string };

function branded(tag: string): Tagged {
  return { tag, [BRAND]: true } as Tagged;
}

function main() {
  const a = branded("div");
  const b: Tagged = { tag: "p" };
  console.log(a, b, BRAND in a, BRAND in b); // { tag: 'div', [Symbol(brand)]: true } { tag: 'p' } true false
  console.log((b as any)[BRAND] === true, { ...a }); // false { tag: 'div', [Symbol(brand)]: true }
}
```

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
until the program ends. A symbol brand added with `Object.defineProperty(o, KEY, …)` is an
optional symbol-keyed field of `o`'s type ([`Object` built-ins](#object-built-ins)).
**Planned**: `PropertyKey` (`string | symbol`) for `Reflect` and proxies
([#823](https://github.com/velt-lang/velt/issues/823)).

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
  made by one loop differ). `indexOf`, `includes`, `Set` elements and `Map` keys agree with
  `==` (`includes` and keys also find `NaN`, as in JS), except that `Map` and `Set` keys of
  a struct type compare by content. Comparing costs only the programs
  that do it: there, an arrow without captures gets an empty environment of its own when it
  is created, and a struct converted to an interface value that is compared is counted, so
  the interface value refers to it rather than to a copy.

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
`x !== undefined` narrows it to `T | null`, `x !== null` to `T | undefined`, and both tests (or
`x != null`, or truthiness) narrow it to `T`. Compared with a `T | null` or a `T | undefined`,
`===` tells the nothings apart and `==` does not, as in Node. A conditional with both a `null`
and an `undefined` arm (`i === 1 ? null : i === 2 ? undefined : i`) is a `T | null | undefined`.
A generic union whose type argument brings a nothing (`T | string | undefined` at
`T = number | null`) holds it like the union written out (`number | string | null | undefined`):
`== null`, `===`, `??` and narrowing see it, inside the generic function too. At `T = unknown`
(or `any`) such a union (`T | string`, `T | Mod<T>`) holds any value: one that is no other
member's is its `unknown` member's (`null`, `0`, an object literal of no member's shape: other
fields, values its fields cannot hold, judged for literals and for variables and field paths by
their types, or a discriminant no member has), and null tests (also `a == b` of a `null` and an
`undefined`), truthiness (`!x`, `if (x)`, `&&`, `||`), `??`, `??=`, `?.`, `typeof` (also
`typeof x === "number"` and `switch (typeof x)`), printing and `JSON.stringify` ask that value
itself, as they do a plain `unknown`. Where `typeof x === "string"` holds, `x` reads as a
`string` also when generic code put the string in the `T` member (`s<T>(v)` with `v: T` passed
to `s<T>(x: T | string)`). `a == b` of two values of a type parameter's type (also
`T | null | undefined`) compares with JavaScript's coercions at `T = unknown` or `any`
(`1 == "1"`), as `==` on `any` does. An `any` or `unknown` converted to it (`const n: number | null | undefined = v`, `v as T | null |
undefined`) gives `null` for a `null` and `undefined` for an `undefined`. Converted to a
`T | null` or a `T | undefined`, it gives the nothing that type has, and stops with a
`TypeError` for the other one (`const n: number | null = v` with a `v` holding `undefined`), as
for any other value the type cannot hold: Node would keep the `undefined`. `undefined`
alone is a type too (its only value is `undefined`), and an
`undefined` with no type to take has it (`const u = undefined`, `[1, undefined]` is a
`(number | undefined)[]`), and a conditional of `null` and `undefined` with no type to take is
a `null | undefined` (`const d = c ? null : undefined`). A `let x: T | undefined;` without an initializer starts as
`undefined`, and so do `let x: any;` and `let x: unknown;`. `null` alone is a type as well, as in TypeScript's discriminated unions where a
field is always `null` in one member (`{ st: "idle"; value: null } | { st: "ready"; value:
number }`).

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
and narrows nothing, as in Node. tsc accepts it; Velt warns (``this `=== null` is always false:
`string | undefined` is never `null` ``), since it is usually a test of an optional parameter or
field written for `null`, which is `undefined` when left out: write `x == null` to test for
either nothing, or `x === undefined`. `switch` compares like `===`. `typeof x === "undefined"`
narrows, `?.` on a value that may be `undefined` gives `undefined` (a `T | null` result
becomes `T | null | undefined`), and `JSON.stringify` leaves out a field
holding `undefined` and writes an array element as `null`. A destructuring default applies where
the value is `undefined` (an `undefined` element or field, an absent optional field, a too short
array), never to a `null` a field or element is declared to hold: `const { a = 1 } = o` keeps
the `null` of a field `a: T | null`, and of an optional `a?: T | null` that holds `null`. `sort(cmp)`
and `toSorted(cmp)` on a `(T | undefined)[]` never pass an `undefined` element to `cmp` and put
those elements last. `return undefined;` in a `void` function is `return;`, and `return void e;` (or an arrow body `(): void => void e`) is `e;` then `return;`. `T | void` is
`T | undefined` (a `void` function's value is `undefined`), so a callback typed
`(e: Error) => boolean | void` may return a `boolean` or nothing. A function whose result type
allows `undefined` returns it from a bare `return;` and when it reaches its end; one whose result
type is inferred, that returns values and also `return;` or can reach its end, returns
`T | undefined`, as in TypeScript. `null` where `T | undefined` is expected, and `undefined` where
`T | null` is, are errors (TS2322), also for a `T | undefined` value where a `T | null` is
expected and the reverse.

A parameter's default applies where the argument is `undefined`, as in JavaScript:
`f(undefined)` and `f(v)` with a `v: number | undefined` that holds `undefined` give
`function f(x = 5)` its `5`. A parameter whose type allows `undefined` may have a default
too (`function f(x: number | undefined = 5)`, or `status = 302` in a method whose interface
declares `status?: number`); it is a `number` in the body.

```ts
function label(n: number = 1, unit: string = "px"): string {
  return `${n}${unit}`;
}

function inc(n: number | undefined = 7): number {
  return n + 1;
}

const found = [1, 2].find((x) => x > 5); // number | undefined
console.log(label(found), label(undefined, "em"), inc(), inc(undefined), inc(1)); // 1px 1em 8 8 2
```

Not supported yet, each with a precise error ([issue #930](https://github.com/velt-lang/velt/issues/930)):
`null | undefined` alone as a written type, `JSON.stringify`
of a value that may itself be `undefined` and `JSON.parse` into a type holding `undefined`;
`x === undefined` and `typeof x === "undefined"` on a `T | null` (its `null` may be JavaScript's
`undefined`, see below; `x == null` tests for either) and `case null` / `case undefined` with
the nothing the type lacks; `as` between `T | undefined` and `T | null`; a parameter default
other than `undefined` on a parameter whose type allows both nothings (`T | null | undefined`);
and a type argument whose nothing differs from one the generic keeps
of it, directly or through what it calls (a `T | undefined` for a generic function holding a
`T | null`), which includes a generic
alias applied that way (`type Nullable<T> = T | null` as `Nullable<number | undefined>`). The
other way round is supported: a generic holding a `U | undefined` at `U = T | null` holds a
`T | null | undefined`. A `Map` or `Record` value type may allow either nothing or both
(`Map<string, number | null | undefined>`, `Record<string, V | undefined>`): an entry holding
`null` or `undefined` is present (`has`, `size`, `Object.keys`, iteration and `console.log`
list it) and reads back as what it holds, a missing key reads `undefined`, and
`JSON.stringify` leaves out an entry holding `undefined` and writes `null`, as Node does.
`JSON.parse` into a type holding a `T | null | undefined` (`as Record<string, V | null |
undefined>`) reads each value as its `T | null`, since JSON has no `undefined`; into a
`T | undefined` without `null` it is not supported yet.
Also not supported yet, although TypeScript accepts them:

- a type argument that allows `undefined` for generic code that sorts an array of its type
  parameter (`xs.sort(cmp)` inside `function f<T>(xs: T[], …)`): an `undefined` element would
  reach the comparator;
- `JSON.stringify` of a value holding a class whose `toJSON()` may return `undefined` (its
  own or a subclass's).

### The standard library

Where TypeScript's lib.d.ts says `undefined`, so does Velt's standard library, as in Node: a
missing key of a `Map` (`m.get("z")`) or of an open `Record` (`r["z"]`), `find` / `findLast`
with no match, `pop` / `shift` of an empty array, `at` out of range (arrays, strings, typed
arrays), the `value` of a finished iterator's result, `WeakMap.get`, `WeakRef.deref` of a freed
target, `Symbol.keyFor` and `description` of a symbol without one, and an unset
`process.env.X`. Each gives a `T | undefined` (a `T | null | undefined` when `T` holds `null`:
`m.get("a")` of a `Map<string, number | null>` tells a stored `null` from a missing key).
Velt's own collections follow the same rule (`Deque`, `PriorityQueue`, `SortedMap`). APIs whose
lib.d.ts result is `null` keep it: `RegExp.exec`, `String.match`, `Headers.get`,
`URLSearchParams.get`.

```ts
const m = new Map<string, number | null>([["a", null]]);
console.log(m.get("a"), m.get("z"), m.get("z") === undefined); // null undefined true
const xs = [1, 2, 3];
const none: number[] = [];
console.log(xs.find((x) => x > 5), none.pop(), xs.at(9));     // undefined undefined undefined
```

Indexing past the end of an array (`xs[9]`) still panics: TypeScript types it as `T`, not
`T | undefined`. The helpers `isNull`, `unwrap`, `unwrapOr` and `map` work on these results as on
a `T | null` ([prelude](../std/prelude.md)).

### Optional values

Optional fields and parameters (`a?: T`) hold `undefined` when left out, as in TypeScript, and
so do the standard library's lookups (see above). `??`, `?.`, `!` and `x != null` treat both
nothings alike:

- `x ?? d` (default; `x ?? null` of a `T | null | undefined` is a `T | null`), `x?.f` /
  `x?.m()` (optional access; the result may be `undefined`),
  `if (x != null) { … }` and early exits narrow `x` to `T` (a local or a field path of one,
  see below), and so does a truthy chain that goes on after `x?.` (`if (x?.m.has(k))
  x.m.get(k)`, `if (x?.ok())`); `switch` supports `case null` and `case undefined`.
  The type of `x ?? d` is `x`'s non-null type when `d` converts to it, else `d`'s type when
  `x`'s non-null type converts to that, else their union, as in TypeScript: with
  `n: number | null`, `n ?? "none"` is a `number | string`. Where an `any` (or `unknown`) is expected
  and one side is an `any`, it is an `any` (see `||` in [Variables](variables.md)).
- `x ??= d` assigns `d` when `x` is `null` or `undefined` and narrows `x` (likewise `x ||= d`
  and `x &&= d`). Its value is `x` afterwards, which is not null when `d` is not, also for a
  module-level variable (`(handlers ??= []).push(h)`). The target may not call a function yet
  (`m[key()] ??= v`): store the key in a variable first.
- `x!` is `x` known to be neither `null` nor `undefined` (TS's non-null assertion). TypeScript
  trusts it; Velt checks it: a nothing panics with `non-null assertion failed`. A write into a
  tuple behind `!` or `as` (one that keeps the tuple's type, or a union's tuple member; an
  element-wise tuple assertion makes a new tuple, see [`as`](#numbers)) changes the tuple in
  place, wherever it is held: a variable, a field,
  an element (`h.t![0] = v`, `xs[i++]![1] = "q"`, `g[r][c]![0] += 1`), a tuple inside such a
  tuple (`g[0]![0]![1] = 9`), a field read through an interface (`iface.opt![0] = 3`,
  `is[q++].t![0]++`), a union member (`(u as [number, string])[0] = 9`) or the
  object a conditional or a call gives (`(c ? a : b).t![0] = 5`, `ps.find(f)!.t![1] = "x"`).
  Every index is evaluated once, before the right side. If the right side (or another task
  during its `await`) sets the field to `null`, the program panics with `the tuple this
  assignment writes into was set to null while its right-hand side ran`, where Node writes into
  the tuple the field held before; a right side that changes a union to another member panics
  with `type assertion failed`.
- `a?: T` reads as `T | undefined`, and `a?: T | null` as `T | null | undefined`. An optional
  parameter `b?: T` is `b: T | undefined = undefined` ([Functions](functions.md)): callers may
  leave it out or pass `undefined` (or a value that may be `undefined`), but not `null`
  (TS2322), and it cannot also have a default.
- An optional field of a class without an initializer is an own property holding `undefined`,
  as in Node with ES2022 class fields: `console.log` shows `a: undefined`, `JSON.stringify`
  leaves it out, and `"a" in o` is `true`. It can be set to `null` only when declared
  `a?: T | null`; it then prints `null` and `JSON.stringify` writes `null`. A spread always
  copies it, set or not (in a generic function too): over an earlier value of a field that can
  hold `undefined` its `undefined` overrides, as in Node, and over a required field (`{ ...u,
  ...c }` with `u.n: number`) an unset one stops the program with the `TypeError` below.
- An optional field of an object type or of an interface with only fields keeps an absent key
  apart from a key holding `undefined`, as Node does. An absent field is not printed by
  `console.log`, and `"a" in o` is `false`; `{ a: undefined }` is allowed, prints
  `{ a: undefined }`, and `"a" in o` is `true`. `JSON.stringify` leaves out both, and writes a
  present `null` (of an `a?: T | null`). A spread (`{ ...a, ...b }`) copies a field `b` has over
  an earlier value, also one holding `undefined` or `null`, and keeps the earlier value where `b`
  lacks the field (`update(u, { deletedAt: undefined })` below clears `deletedAt`, as in Node).
  Where the target field is required and cannot hold `undefined` (a `Partial<U>` spread back into
  a `U`), a source that lacks the field keeps the earlier value, and one that holds `undefined`
  stops the program with a `TypeError` (``a spread copies `undefined` into a field of type
  `string` ``): Node would copy the `undefined`, which the field's type can't hold. Without a
  target type the field has the earlier part's type, as TypeScript types it (`{ ...u, ...p }`
  with `u.n: number` and `p.n?: number` has `n: number`), so the same holds there.
- An object literal may leave out an optional field (`{ port: i64; host?: string }` accepts
  `{ port: 80 }`). A field of an object type declared `a: T | null` may be left out too, and is
  then `null`.
- In an object type, `?` is part of the type, as in TypeScript: `{ a?: string }` and
  `{ a: string | undefined }` read alike but are different types (the first may be absent),
  with no implicit conversion between them; copy with `{ ...x }`.
  Difference from TypeScript: TypeScript also accepts a `{ a: F | undefined }` where a
  `{ a?: F }` is expected; in Velt the two are different types (the second keeps a presence
  flag), so copy with `{ ...x }` there.

```ts
type User = { name: string; deletedAt?: string | null };

function update(u: User, patch: Partial<User>): User {
  return { ...u, ...patch };
}

const u: User = { name: "ann", deletedAt: "2026-01-01" };
console.log(JSON.stringify(update(u, {}))); // {"name":"ann","deletedAt":"2026-01-01"}
console.log(JSON.stringify(update(u, { deletedAt: null }))); // {"name":"ann","deletedAt":null}
console.log(update(u, { deletedAt: undefined })); // { name: 'ann', deletedAt: undefined }
const v: User = { name: "bo" };
console.log(JSON.stringify(v), "deletedAt" in v); // {"name":"bo"} false
console.log(update(v, { deletedAt: null })); // { name: 'bo', deletedAt: null }
```

- `JSON.parse<T>`: an absent key leaves an optional field absent (`undefined`). An explicit
  `null` for an `a?: T` field, whose type has no `null`, leaves it absent too (a Velt rule: the
  type cannot hold the `null`); for an `a?: T | null` field it is a present `null`. The
  TypeScript forms that take the type from the context, `JSON.parse(text) as T` and
  `const t: T = JSON.parse(text)`, reject that `null` instead (`expected string at $.a`), as
  they reject a value of another type. A field
  declared `a: T | null` (not optional) may be missing, and is then `null`, as for an explicit
  `null`. A `JsonValue` always tells them apart: `v.has("a")` vs `v.get("a")?.isNull()`.
- `x?.a.b` short-circuits the rest of the chain like TypeScript (`undefined` when `x` is `null`
  or `undefined`; `.b` is never evaluated). This holds for a `T | null` `x` too, as in
  JavaScript: with `u: User | null`, `u?.name` is a `string | undefined`, and a rest of type
  `T | null` gives `T | null | undefined`. Parentheses end a chain: `(x?.a).b` needs `x?.a` to
  be neither `null` nor `undefined`.
- Narrowing applies to locals and to field paths of locals (`this.x`, `node.left`), like
  TypeScript; assigning a non-null value narrows too (also in the declaration: `let x: string
  | null = "a"`). After an `if` or a `switch`, what holds is what each way out ends with: the
  facts of the condition a branch started from unless it reassigned the variable, and what its
  own statements established (`if (x == null) { x = d; }` narrows `x` after it; `if (x ==
  null) return; else x = null;` does not; a `case` passes on what holds at its `break`; after a
  `while` loop without `break` its condition is false: `while (y === null) { y = next(); }`). A
  narrowed field is re-checked when read, so a call that set it to `null` in between panics
  instead of reading `null` (`a narrowed field was set to null before this read`, `undefined`
  for an optional field). An optional member of function type can be called once a check
  narrowed it (`if (o.onError) o.onError(e)`, `if (def?.transform) def.transform(v)`). Inside
  a closure, a variable narrowed where the closure is created stays narrowed when the function
  assigns it nowhere after that (nor in a loop around the closure), as in TypeScript: the
  closure may run after such an assignment. It stays narrowed in the whole closure body, also
  after a branch or loop where the closure first names it.
- `x == null`, `x != null`, `x === undefined` and `x ?? d` are allowed on a value whose type has
  neither `null` nor `undefined`, as in TypeScript (a defensive check): the test is `false`
  (`!=`: `true`) and `d` is not evaluated. On an element read of an array, a string or
  a typed array (`xs[i] ?? d`, `xs[i] == null`) the index is checked instead, so past the end the result is `d` (`true`),
  as in JavaScript, where reading past the end gives `undefined`.
- A null check (`!= null`, `!== undefined`, truthiness) of a variable that a closure assigns
  narrows it in the function that makes the check until the next assignment there, as in
  TypeScript (`if (cleanupFn) cleanupFn();`). A call in between may run the closure, so each
  such read checks the value again and stops with a `TypeError` if the closure set it to `null`
  (JavaScript throws one where it calls or reads through the `null`). The closures that capture
  the variable do not see the narrowing. Other tests (`typeof`, `instanceof`, union members)
  do not narrow such a variable, where TypeScript keeps the narrowing: the variable may no
  longer hold what was checked. Test a `const` copy instead, which nothing can reassign; the
  error at such a use says so:

  ```ts
  class Conn {
    send(msg: string): string { return `sent ${msg}`; }
  }

  let conn: Conn | string = new Conn();
  const close = () => { conn = "closed"; };
  const c = conn;                 // `if (conn instanceof Conn) { conn.send(…) }` is an error
  if (c instanceof Conn) {
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
  nickname?: string;              // starts as undefined
}

function greet(name: string, greeting?: string): string {
  return `${greeting ?? "Hello"}, ${name}`;   // greet("ann") works: greeting is undefined
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
- `e as const` is accepted and keeps the value as it is. An array literal of strings keeps
  their literal types and is read-only, like TypeScript's `readonly ["a", "b"]`:
  `const NAMES = ["a", "b"] as const` is an array of `"a" | "b"` (and `["a"] as const` of `"a"`), so
  `for (const n of NAMES) f(n)` and `f(NAMES[0])` pass a `"a" | "b"` parameter, and a
  `readonly string[]` parameter or variable, an `Iterable<string>` parameter and the array
  `new Set<string>(…)` copies from take it, or any array of a literal type (as a copy of strings, which the callee cannot tell
  apart, since it cannot write through the parameter). Other values need no `readonly` or
  literal-type annotation for it.

## Union types

`A | B | C` of any types is a tagged union. Member order and nesting don't matter; `T | null` is
the nullable type; `void` cannot be a member.

- **Widening** is implicit: a member value (also a subclass of a class member, or a value
  implementing an interface member) converts to the union, and a union converts to a wider
  union. An array literal with no type from context whose elements have different types
  is an array of their union, as in TypeScript (`[1, "a"]` is a `(number | string)[]`, and
  `[a, d]` of a `number | undefined` and a `string | null` holds all four). `null` and
  `undefined` elements make it allow them (`[1, null]` is a `(number | null)[]`), a nested
  array literal keeps its own type (`[[1], ["a"]]` is a `(number[] | string[])[]`), and
  `[...xs, true]` adds `boolean` to the spread arrays' element type. A literal takes the member it belongs to; add a suffix (`5i32`) when several number
  members match.
- **Narrowing**: using a member's fields or methods needs that member, except `x.toString()`,
  which every member has: it gives what `String(x)` does (a class's own `toString()`, a
  primitive's text), also as `x?.toString()` on a union that may be `null`.
  - `typeof x === "string" | "number" | "boolean" | "symbol" | "object" | "function"` (and
    `!==`): all number types are `"number"`; classes, structs, arrays, maps and `null` are
    `"object"`; closures are `"function"`. An impossible tag is an error. On a value that is
    not a union, a test it always passes leaves it `never` where the test fails, when its type
    fixes the answer (numbers, strings, booleans, symbols, functions, classes, structs, arrays,
    maps); not for `object`, which may hold a function, nor for interfaces with methods or
    enums. On a value whose type is a type parameter (also `T | undefined`), `typeof` tells the
    instantiation's value at run time, and `x === undefined` / `x == null` too (`T` may be
    `undefined` or `null`); for a variable, neither boxes the value.
  - `x instanceof C` matches members whose class is `C` or a subclass. A member of a base
    class of `C`, or an interface value, is tested at run time and narrows to `C`
    ([downcasts](classes.md#instanceof-downcasts)). `x instanceof Promise` matches the
    promise member (`View | Promise<View>`), when no class named `Promise` is in scope.
  - `x == literal` / `x != literal` selects the literal's member.
  - `"key" in x` keeps the members that may have the key (declare it, optionally or not) and
    `!("key" in x)` those that may lack it ([the `in` operator](#the-in-operator)). On a
    narrowed `x` only the members left count (`typeof x === "object" && "root" in x` on a
    union with `string` and `boolean` members).
  - `typeof` tests narrow a field path of a local too (`typeof node.text === "string"`,
    `this.label`, `h.node.text`), as in TypeScript, until the path or a part of it is
    assigned. A call between the test and a read may change the field, so each read checks
    the member again and stops the program if the field now holds another member (Node would
    go on with the new value). A field narrowed to its function member can be called
    (`typeof p.f === "function" ? p.f() : p.f`), and one narrowed to some of its members reads
    as the union of them (with its `null` or `undefined` when no test ruled that out).
  - A `U | null` variable narrowed to some of `U`'s members (`typeof x !== "number"`, the
    `false` branch of a type predicate) is a value of the union of those members with its
    `null`: `return x` where the return type is `VNode | string | null`.
  - Conditions of `if`, `while`, `&&`, `||`, `!`, ternaries and early exits narrow a local
    until it is reassigned; `a || b` narrows to the members either side allows
    (`typeof c === "string" || typeof c === "number"` leaves `c` a `string | number`);
    `switch` narrows each case ([`switch`](control-flow.md#switch)).
    A local that a closure assigns is not narrowed by these tests ([Null](#null-and-undefined)).
  - Assigning a member's value narrows to that member until the next assignment or join, also
    in the declaration (`let u: string | number = "x"; u.length`, `else { p = new B(); }
    p.b`), as in TypeScript.
- Printing and template literals show the active member's value. A union with a member that
  cannot be printed (a closure) prints once a test has narrowed it to members that can
  (`typeof v !== "function"`). `JSON.stringify` works on
  unions; `JSON.parse` decodes them when the JSON value tells the members apart (discriminated
  unions by their discriminant; see [`velt:json`](../std/json.md)).
- A union of numbers, bools, strings and literals is copied; one holding an object refers to
  it like any other variable.
- A union alias may name itself behind an indirection: as an array's element, an index
  signature's or a `Map`'s value, or in a function type, as JSON-like types do; its members
  may include `null`, `undefined` or both (`type Child = Node | string | null | undefined |
  Child[]`). Elsewhere
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

A union of object types (anonymous object types, structs, classes or interfaces) whose members
all have a field of a literal type is a **discriminated union**; that field (any name; `kind`
below) is the *discriminant*. It is compiled like a Rust enum: the discriminant is the tag, not a
stored string.

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
- Members may be interfaces with methods, and intersections of one with object types
  (`Base<T> & { state: "errored" } & ({ value: T; hasValue: true } | { value: null; hasValue: false })`,
  one member per part of the inner union). A field every member has reads through each one's
  getter, and a method every member has is called on the union: through an interface every
  member extends that declares it, else each member's own when they have the same type. Members
  may be classes too (`Circle | Sq` with `area()` on both, or a union of classes only): each
  member's own method runs on the value itself: the receiver first, then the arguments in order,
  each evaluated once. The call's type is the union of the members' results (`number | string`
  when `scale()` returns a `number` on `Circle` and a `string` on `Sq`), as in TypeScript.

```ts
interface Base {
  describe(): string;
}
interface Circle extends Base {
  readonly kind: "circle";
  readonly r: number;
}
interface Square extends Base {
  readonly kind: "square";
  readonly side: number;
}
type Shape = Circle | Square;

const shapes: Shape[] = [
  { kind: "circle", r: 1, describe: () => "round" },
  { kind: "square", side: 2, describe: () => "four corners" },
];
for (const s of shapes) {
  console.log(s.kind, s.describe()); // circle round, then square four corners
  if (s.kind === "square") console.log(s.side); // 2
}
```
- **A live object as a union of interfaces (a union view).** When every member is an interface
  with methods and the members share an interface (`Base` above), one object whose getters
  change can be cast to the union, as libraries build such state machines
  (`impl as AsyncState<T>`): `wide as Shape` keeps the same object, and every discriminant
  test, `switch` and field read on the union calls its getters again, so the union sees the
  object change, as in Node. The cast needs the object's type to have every member's fields,
  each typed as the union of the members' types (`kind: "circle" | "square"`), and the methods
  of the shared interface (a class instance converts through an interface its class
  implements: `new Wide() as Shape`); else it is an error. A read after a test is checked
  against the member the test selected: an object whose state no longer fits it (`kind` is
  `"circle"` but `r` is `null`) gives a `TypeError`, never a value of the wrong type. Values built from
  literals keep their member and its tag, and can sit next to the live object in one variable
  or array. Using a narrowed union that may hold such an object as a value of its member type
  (passing it where a `Circle` is expected) is not supported yet (an error); read its fields
  instead, or pass it where the union is expected.

```ts
interface Base {
  describe(): string;
}
interface Circle extends Base {
  readonly kind: "circle";
  readonly r: number;
}
interface Square extends Base {
  readonly kind: "square";
  readonly side: number;
}
type Shape = Circle | Square;

interface Wide {
  readonly kind: "circle" | "square";
  readonly r: number;
  readonly side: number;
  describe(): string;
}
let kind: "circle" | "square" = "circle";
const wide: Wide = {
  get kind() {
    return kind;
  },
  r: 1,
  side: 2,
  describe: () => "changes",
};
const live = wide as Shape;
console.log(live.kind, live.describe()); // circle changes
kind = "square";
if (live.kind === "square") console.log(live.side); // 2
```
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
  An object type that is no member narrows, when the call is true, to the members whose values
  have every required member of it: `v is { length: number }` on `string | number` to `string`
  (strings, arrays and functions have `length`), and `v is Computed<unknown>` on a `JSXChild`
  (primitives, arrays and object types without `value`) to nothing, as TypeScript's
  intersection with them has no value Velt can hold: the code there type-checks (`v.value`) and
  does not run, and a predicate that returns true anyway panics as above. A union with an
  interface value or a class instance isn't narrowed. When false, the variable is left as it
  is.
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
  value lacks (`x.c = "s"` on an `{ a: number; c?: string }`; a write through `any`, `x.a = 1`
  or `x[k] = v` with `x: any`, counts only when the program converts the copy, its original or
  a value that may hold one to `any`, `unknown` or `object`, since only such objects are reached
  through one), compares values of the
  expected type with `===` (also as `A | null` or another union holding it), prints, serializes or lists the keys of a value holding the
  expected type, spreads a value of the expected type (`{ ...x, c: 3 }`; Node would show
  or copy the original's fields, in its order), or tests a value of the expected type with `in`
  for a key the copy left out (`"b" in a`, which Node finds on the original), directly or in generic code it calls (`xs.indexOf(x)` and `xs.includes(x)` compare with `===`).
  Keys compared by identity count as `===`: a `WeakMap`, `WeakSet` or `WeakRef` of the
  expected type. So does converting a value that may be the copy (followed through variables,
  parameters and results; anywhere once the copy is stored in a field or an array) to `any`,
  `unknown` or `object` when the program compares two such values that may be objects
  (`(a as any) === (ab as any)`, not `x !== undefined`), keys a `Set<any>`, `Map<object, V>`
  or weak collection by them, or prints, spreads or lists the keys of one. An array
  converts element by element only when fresh, like [wider element
  types](#objects-arrays-tuples-and-maps): `const ns: Named[] = roster();` for a `roster()`
  that returns a new `(Named & Scored)[]`; copy another one with
  `xs.map((p) => ({ name: p.name }))`.
- **Extensions with optional fields can share the layout**: for an interface that extends one
  object type (an interface with only fields, or an object alias that refers to itself) and
  adds only optional fields the base lacks, as in `interface InternalVNode extends VNode {
  _ns?: boolean }`, a program that converts between the two (`vnode as InternalVNode`, passing
  a `VNode` where an `InternalVNode` is expected or the reverse, also as `VNode[]` and
  `InternalVNode[]` or as the values of a `Map`) converts the same object, as in TypeScript: a
  field written through either type is there through the other, and `===` holds. For that,
  every object of the base's layout gets room for the extensions' fields, absent until
  written: in a tree of 1,048,575 four-field `VNode`s with an extension adding five optional
  fields (`bench/extended_layouts_mem.sh`), an object grows from 88 to 144 bytes and the peak
  memory from 198.7 MB to 214.8 MB. An extension the program only declares (often one in a
  dependency) costs nothing. Each type keeps its own fields: `VNode` has no member `_ns`, and
  `keyof VNode`, mapped types (`Required<VNode>`) and structural conversions see `VNode`'s
  fields only. Printing and `JSON.stringify` show the whole object, an added field once it is
  written (one holding a function is printed, and left out of JSON, as in Node); `Object.keys`,
  `for...in`, spreads and `in` list the fields of the type the program looks through, so one of
  those on a `VNode` is an error once the program may give its objects an added field: assigns
  it, writes it in an object literal of an extension (`{ ...v, _ns: true }` too), or makes
  such objects from text or a dynamic value (`JSON.parse`, `x as VNode` of an `any`), whose
  keys are not known (look through `v as InternalVNode`). Several such extensions of one base share it while they give a field
  of one name the same type; one that gives another type (`_subTree?: VNode` next to
  `_subTree?: VNode | null`) keeps its own layout and converts by copying, as above, the
  extension nearest to the base (by module path) sharing it. Neither the base nor the extension
  may be generic. As for other optional fields written after construction, printing lists them
  in declared order (the base's fields, then each extension's), not in the order they were
  written (#1319).
- An alias may refer to itself through `&` when its parts are object types written out:
  `type Tree = { kids: Tree[] } & { v: number }` is the interface with the fields `kids` and
  `v`. A part may also name a non-generic alias of such a type, or an interface with only
  fields, declared in the same module (`type Tree = Base & { kids: Tree[] }` with
  `type Base = { v: number }` or `interface Base { v: number }`). A field in two parts is one
  field when its type is written the same in both (`Base & { v: number; kids: Tree[] }`,
  also for function types; not yet for index or call signatures). Object aliases may also
  refer to each other (`type A = { b: B | null }; type B = { a: A[] }`, or through a named
  part or an alias that renames another): each is an interface. An object alias that names
  itself is one whatever other aliases name it back (`type Visitor = (t: Tree) => void`). A
  recursive alias is an error otherwise: with a part that is a type parameter, a generic alias
  (`Box<number> & { … }`), an alias or interface of another module, an interface with methods,
  or a field of two types; the error points at that part.

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

A part may be a **type parameter** (`T & { $set: (v: T) => void }`, `TMain & TSub`), in a
generic alias or written in a generic signature: the intersection is worked out where the type
arguments are known, so a caller sees the object type with both parts' fields, and a type
argument that can't be a part (a class, say) is reported there. A type argument inferred
through such a parameter comes from the fields the object part names (`T = { n: number }` for a
`Signal<{ n: number }>`), else from the argument's parts the pattern doesn't name (`T = A`
for a value declared `A & { id: string }`), else from the whole argument
(`T = { a: number; id: string }` for `{ a: 1, id: "x" }` against `T & { id: string }`), as in
TypeScript. Such an argument is inferred from before the other arguments are checked, so
`reset(s, { st: "done", data: null })` with `reset<T>(s: Signal<T>, v: T)` checks the literal
against the `T` that `s` fixes. `any`, `unknown` and
`object` parts intersect as in TypeScript: `any & X` is `any`, `unknown & X` is `X`, and
`object & X` is `X` when `X` is an object type (so `T & object` at an object type `T` is `T`).
`object & X` keeps only the members of `X` that are objects, and has no value (`never`) when
none is: `T & object` at `number` is `never`, so sigx's `PrimitiveSignal<T> | Signal<T & object>`
at a `number` is `PrimitiveSignal<number>`; `never & X` is `never`. In a generic body,
`T & object` is `T` under a `T extends object` bound (an overload returning `Signal<T>` fits an
implementation returning `Signal<T & object>`), and a value of `T & object` (or of an
intersection with such a part) is an `object`, so it can be a `WeakMap` key. So is an
intersection with a part that is an object type and no part that is a type parameter
(`{ value: T } & { $set(v: T): void }`): it satisfies `T extends object`. `T & { __brand: "Id" }`
with an unbounded `T` is not an `object` (at `T = string` it is a branded `string`).

**Type-level forms and keys in generic code.** A mapped type, `T[K]`, `keyof T` or a utility
type over a type parameter (`Partial<T>`, `Pick<T, K>`, …) may be written in a generic function's
signature or body, not only in an alias: it is evaluated at every instantiation, as TypeScript
evaluates it there. A key parameter `K extends keyof T` is inferred as the literal key the call
passes (the union of the keys when several arguments pass one: `swap(o, "x", "y")` has
`K = "x" | "y"`), and `x[k]` reads or writes that field of `x: T`, the field itself (an array
read through it is `===` to the original; a write reaches the object, a value a call returned
too: `first(os)[key()] = v`). The object and the key may be any expressions, each evaluated
once, in order. An optional field reads as `F | undefined`, and `typeof`, `=== undefined` and
`== null` on a `T[K]` value test the instantiation's value (`typeof o[k]` is `"undefined"` for an
absent optional field, `"number"` for a present one). A key type of several keys reads or
writes the field the key holds when those fields have one type (`k: "x" | "y"` on
`{ x: number; y: number }`); keys whose fields have different types are not supported yet, an
error at that instantiation. An error at an instantiation is reported in the generic function,
where the failing form is, with the call that instantiates it and the type arguments (and each
call on the way, when another generic function's body makes the call). A call whose argument
breaks a type parameter's constraint (`get(p, "w")`, a key `p` lacks) is one error, at that
argument, as in TypeScript, and is not instantiated. An alias
declared in a generic function's body may name the function's type parameters.

```ts
function get<T extends object, K extends keyof T>(source: T, key: K): T[K] {
  return source[key];
}
function set<T extends object, K extends keyof T>(target: T, key: K, v: T[K]): void {
  target[key] = v;
}
function update<T extends object>(x: T, patch: Partial<T>): T {
  return { ...x, ...patch };
}

const st = { count: 0, name: "a" };
set(st, "count", get(st, "count") + 2);
console.log(st.count, JSON.stringify(update(st, { name: "b" }))); // 2 {"count":2,"name":"b"}
```

**Spreads in generic code.** `{ ...t }` and `{ ...t, tag: "x" }` with `t: T` build a new object
at each instantiation, as Node copies an object's own properties: the fields of `T`'s type
argument, then the explicit ones (a later field overrides an earlier one and keeps its position),
with nested objects and arrays shared rather than copied. The literal is a `T`, or a
`T & { tag: string }`, as in TypeScript, so explicit properties are checked against the expected
type (`$set: (v) => …` gets its parameter type from `Signal<T>`). An instantiation whose `T` is
not an object type (`{ ...t }` at `T = number`) is an error there. An optional field of a later
part overrides an earlier value of it only while present, as in Node: an absent `name` keeps the
earlier one. A field required in an earlier part keeps its type under a later optional one
(`n: number`, as TypeScript types it; both parts must give the field one type), so, as for a
concrete spread, a present `undefined` over it is a `TypeError`: Node keeps `undefined` in a
field typed `number`, which Velt can't represent. A spread used directly as a `T`
(`return { ...x, ...patch }` from a function returning `T`) is built as `T` the same way. A source
whose type argument is an interface with methods is an error at that instantiation: a spread
copies an object's own properties, which are its methods for an object literal but not for a
class instance, and an interface value may be either. A source of a concrete type that may be
`undefined` or `null` (`...v` with `v: V | undefined`) spreads nothing while missing, as in Node;
one whose type has a type parameter (`x: T | undefined`) is not supported yet (#350): test it
first (`x === undefined ? {} : { ...x }`).

```ts
type Signal<T> = T & { $set: (v: T) => void };

function withSet<T extends object>(t: T, log: string[]): Signal<T> {
  return {
    ...t,
    $set: (v) => {
      log.push(JSON.stringify(v));
    },
  };
}

const log: string[] = [];
const s = withSet({ n: 1 }, log);
s.$set({ n: 2 });
console.log(s.n, log); // 1 [ '{"n":2}' ]
```

A value of a generic body used as one of its parts (`s: T & { tag: string }` returned as a `T` or
passed as a `{ tag: string }`, `{ ...t }` returned as a `T`) is the same value. TypeScript always
passes the same object; Velt's object types have fixed layouts, so this is allowed only at
instantiations where both are one object type (a `T` that already has `tag`, or `{ ...t }` at any
object type), and an instantiation where they differ (`T = { a: number }`, whose values would
have to be copied, losing identity and writes) is an error at the generic function. `x as T & object`, for an `x` of a type parameter `T` without an
`object` bound, is checked at run time: the value itself (not a copy) when it is an object, a
`TypeError` when it is a primitive, `null` or `undefined` (TypeScript keeps such a value; write the cast after a test
that rules primitives out, as sigx's `signal` does). A generic alias used at
type parameters (`Signal<{ value: V }>` in `function f<V>`) is the object type its intersection
makes, as if written out. So is a generic function's result `Signal<T>` at a `T` that mentions
the caller's type parameters (`signal({ st: "idle", data: d as D | null })` in
`function cell<D>`): its fields are read, written, destructured and reached with `?.`, and
`$set` is called, on the same value. `Omit<T, never>` and `Pick<T, never>` name no fields.

```ts
type Signal<T> = T & { $set: (v: T) => void };

function relay<T extends object>(s: Signal<T>): Signal<T> {
  return s;
}

function stamp<T>(x: T & { id: string }): T & { id: string } {
  return x;
}

let last = 0;
const s: Signal<{ n: number }> = {
  n: 1,
  $set: (v) => {
    last = v.n;
  },
};
const r = relay(s); // Signal<{ n: number }>: `n` and `$set`
r.$set({ n: r.n + 1 });
const t = stamp({ a: 1, id: "x" });
console.log(last, t.a, t.id); // 2 1 x
```

Differences from TypeScript, each a compile error with a note on what to write instead:

- A field that is a type parameter in one part and a type in another (`{ a: D } & { a: string }`)
  is `D` when `D`'s constraint is that type (`D extends string`), the deferred `D & X` when `X` is
  an object type or a type parameter, and an error otherwise. Two function types in one field
  (nested `Signal<Signal<T>>`'s `$set`s) are one function when one of them takes every argument
  the other does (its parameters have a subset of the other's fields); other overloads are
  errors.
- When the parts have no value in common (`{ k: string } & { k: number }`, or two different
  discriminants), TypeScript makes the type, or the field, `never`; Velt reports
  ``no value has type `…`: field `k` is `string` in one part and `f64` in another``.
- Classes and structs are not parts (Velt classes are nominal, not structural): use
  `Pick<C, …>` or a field-only interface. Arrays (also `Signal<T>` at an array `T`, reported at
  the call) and function types (overloads) are not parts either; `T extends A & B` stays a
  bound on two interfaces.
- An interface with methods is a part: `I & { errors: number }` (and `A & B`, or `T & { … }` at
  an interface `T`) is an interface with every part's members, one whatever the order of the
  parts (`A & B` is `B & A`). An object literal implements it, a class that implements each
  interface part, itself or through a base class (and has the object parts' fields, or `get`/`set`
  accessors for them, which reads and writes through it run), converts to
  it as the same object, generic classes too, and a field written through it reaches the class's
  field. Writing through it a field that a class has only a `get` for stops the program with
  Node's `TypeError: Cannot set property n of #<C> which has only a getter` (what strict code
  gets), also when a subclass overrides only the getter of its base's pair. Changing a part of such a
  field through the interface (`x.o.v += 1`) is an error for now (#350): Node changes the object
  the getter returns in place, and Velt would change a copy and write it back through the setter. A generic class's
  field provides a part's field at the instance's type arguments (`val: T` of `Cell<string>` for
  `{ val: string }`). Its values convert to each interface part as the same object (`===` holds). `Pick<I, K>`
  and `Omit<I, K>` of such an interface are interfaces with those members, which values of `I`
  convert to the same way; in general an interface value converts to any interface whose members
  it has (an interface it extends, a structurally equal one, in both directions). Methods of one
  name with different signatures in two parts (overloads in TypeScript) are an error, and so is a
  field in one part with a method of that name in another (no value has both), and converting
  such a value to its object part (`{ errors: number }`), which would copy it. For the same
  reason, a value of an interface that extends a field-only interface `B` does not convert to
  `B`: `B` is an object type with a fixed layout, and TypeScript keeps the same object. `T & object` at an interface or class
  `T` is `T`: `t as T & object` is the same value.
- In generic code, a value of an intersection with a type parameter part (`x: T & { id: string }`)
  is passed on and returned as it is, and the members of its object parts are read (also with
  `?.` and destructuring), written and called on the value itself (`x.id = "b"`, `s.$set(v)` for
  `s: Signal<T>`); its conversion to one part (`T` or `{ id: string }`) and building one
  (`{ ...t, id }`) are not supported yet (#350).
- An intersection is the same type as the object type with its fields written out, so where a
  program has both forms, inference through `T & { id: string }` treats them alike: an object
  literal `{ a: 1, id: "x" }` gives `T = A` when the program also declares a value of type
  `A & { id: string }` (TypeScript: `{ a: number; id: string }`). It differs only where code
  depends on `T` itself, such as `JSON.parse(s)` returned as a `T`.
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
of the fields' types, as in TypeScript: `Person["name"]` is `string`, and an optional field
`email?: string` gives `string | undefined`. On an interface with methods it reads a field or
a getter (`get size(): number` gives `number`). A key that is not a field
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

A function generic only over the errors its callbacks throw (`setTimeout<E>(task: () =>
Promise<void, E>, ms: i64)`) has the function type without them (`typeof setTimeout` takes a
callback that does not throw), as TypeScript declares such functions without type parameters.

Not supported yet (each is an error): `typeof` of another generic function, of an `async` or
generator function (function types have no type parameters), of a class or enum itself (TypeScript's constructor
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
  each member's mapped type. `T[K]` of an optional field is `F | undefined`, as TypeScript
  reads it (inside these forms and outside them).
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
  the instance of a generic interface with only fields. An instance whose field is `void`
  (`Box<void>`, or sigx's `__ref: TRef` brand with `TRef = void`) is accepted, as in
  TypeScript, but the field keeps no value: what is written to it (`ref: null as any`) is
  dropped, and a read of it is `void` (which can't be printed or tested). Printing the object
  directly or writing it as JSON (directly or through a generic) is a compile error, and
  printing it through a generic or an `any`, writing it as JSON through an `any` or `unknown`,
  or reading the field through an `any`, stops with a `TypeError` (#1411), where Node shows the
  value written. A `void` field written in an object type, a struct or a class, or a class
  instance with one (`new Holder<void>()`), is an error.

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
  a method, and getters and setters (`get x() {}`, `set x(v) {}`), are supported where the
  literal implements an interface with methods
  ([Object literals implementing interfaces](#object-literals-implementing-interfaces)) and in
  a literal with no expected type
  ([`this` in object-literal methods](#this-in-object-literal-methods)), not yet in a literal of
  an object type. A generic method (`match<R>(arms: Arms<R>): R { ... }`)
  parses as a property holding a generic arrow, which is not supported yet there (a generic
  arrow must be the value of a `const`, [Functions](functions.md#arrow-functions-and-function-types)). A generator method `*name() { ... }` (`async *name()`) is
  a property holding a [generator function](functions.md#generator-function-expressions), as
  `name: function* () { ... }` is; a generator method with a symbol key only as
  `*[Symbol.iterator]()`
  ([Iterable object literals](#iterable-object-literals)).
- **Method signatures in object types**: `{ size(n: number): number; stop(): void }` declares
  fields of function type, as `{ size: (n: number) => number; stop: () => void }` does, and
  `label?(p: string): string` an optional one (`((p: string) => string) | undefined`, absent
  when left out; call it as `o.label?.(p)`). A generic method signature
  (`run<T>(f: () => T): T`) declares a method in an interface; in an object type, and as an
  optional one in an interface (`wrap?<A>(a: A): A`), it declares a field of generic function
  type (`<T>(f: () => T) => T`), which is not supported yet and is reported where it is used.
  A call signature alone (`interface F { (x: T): R }`, `type F = { (x: T): R }`) declares the
  function type `(x: T) => R`: a value of it is called as `f(x)`, any function or arrow of that
  type is one, and `typeof f` is `"function"`. With members, an object type or interface with
  one call signature is a [function with properties](functions.md#functions-with-properties)
  (TypeScript's `((x: T) => R) & { stop(): void }`); several call signatures (an overloaded
  function) are not supported yet; a generic call signature (`<T>(x: T): T`) is a generic
  function type, planned, and a construct signature (`new (x: T): C`; as in TypeScript,
  `new(…)` in an object type is never a method, while in a class it is) the type of a class
  used as a value, `new (x: T) => C`
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
  ([Consuming an iterable](control-flow.md#consuming-an-iterable)). Elements, arguments and
  properties run in written order, as in JS: `[f(), ...g()]` calls `f` first, an iterable is
  consumed where it is written, and `{ y: f(), x: g() }` calls `f` first whatever the order of
  its type's fields; a property a later spread overrides (`{ a: f(), ...o }`) still runs. A
  spread copies its source's elements or properties where it is written, before later ones
  run: `{ ...o, a: f(o) }` and `[...xs, ...f(xs)]` copy `o` and `xs` before `f` changes them,
  and `[n, ...bump()]` reads `n` before `bump` does.
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
  is called (`const { area } = rect;`). A method or function-typed member destructured out of
  an object or interface value is a function value (`const { insert, remove } = options;`),
  bound to the object it was read from (in JS it is unbound, so the two differ only where JS
  would throw a `TypeError` for a method that uses `this`).
  As in TypeScript, an array pattern over an array literal types the literal as a tuple
  (`const [n, s] = [1, "s"]` gives a `number` and a `string`, also in a destructuring
  assignment, for `[…] as const` and for a `const c = [2, "y"] as const` taken apart, also in
  a closure or function that captures `c` and where `c` is a module constant; `c` itself
  stays a read-only array of the union, with `length`, indexing, iteration and the read
  methods). Taking `c` apart reads its elements, so a write made through a cast
  (`(c as unknown as (number | string)[])[0] = 7`) is seen; an element overwritten with a
  value of another type stops with a `TypeError`. Destructuring an `any` reads each element or property through it (`const [a, b] =
  v` is `v[0]`, `v[1]`, each an `any`, read once; a default applies where the read gives
  `undefined`; a `...rest` of an `any` is not supported yet). A computed key naming a variable
  (`const { [k]: v = 0 } = r`, `({ [k]: p } = r)`, a local, an outer function's or a module
  variable, read when the pattern runs) reads `r[k]`; one naming a module constant holding a
  string or a symbol (`[KEY]`) reads that member, also of an object, interface or class; a
  string, number or template key (`{ ["a"]: v }`, `{ [0]: v }`), like a quoted or numeric
  property name (`{ "a": v, 0: w }`), names that property. On an array or a tuple a numeric
  key reads that element and `length` the length, a tuple's arity (`const { 0: first, length }
  = xs`, also with defaults, nested, also inside a tuple element, `const [i, { 0: c }] = t`, in
  `for...of` and in parameters; `...rest` there is not supported yet, nor
  a numeric key above 4096). Any other key expression
  (`{ [f()]: v }`) is an error: store the key in a variable first. Next to a computed key a
  `...rest` is not supported yet. Defaults on a `Record` or index-signature pattern apply where
  the key is missing (`const { port = 80 } = env`).
- **Object rest** `const { children, ...rest } = props;` (also in `let`, nested patterns,
  next to defaults and in parameters, `function f({ id, ...rest }: P)`): `rest` holds the
  other own properties, in the object's order, as in JS. Its type is TypeScript's: on an object
  type or an interface of fields, the object type of the other fields (`Omit<P, "children">`;
  an optional field is copied only while present); on a class instance, the other fields (its
  methods and getters live on the prototype, so the rest has none, nor `#private` fields); on a `Record<string, V>`,
  a `Record<string, V>` of the other keys. The rest is a new object: its fields are copied
  (a nested object or array is shared, as in JS). Not supported yet (each is an error): the rest of a value
  whose type is a type parameter, an interface with methods, a record with a closed key
  type, or a class with a TypeScript `private` field the pattern doesn't take (JS copies it,
  but the rest's type leaves it out), and `...rest` in `for...of` patterns.

  ```ts
  interface Props { id: number; title: string; hidden?: boolean }
  function main() {
    const props: Props = { id: 1, title: "t" };
    const { id, ...rest } = props;
    console.log(id, rest); // 1 { title: 't' }
  }
  ```
- **Defaults** in `const` and `let` patterns: `const { host = "localhost", port = 80 } = opts;`
  takes the default when the field is absent (optional) or `undefined`, and
  `const [first = 0] = xs;` when the array is too short or the element is `undefined`, as in JS;
  a field or element declared `T | null` keeps its `null`, and so does an optional field
  declared `a?: T | null` that holds `null`. `for...of` patterns take defaults (and an object
  rest) the same way: `for (const { name, port = 80 } of servers)`, and so do parameter
  patterns (`function f({ a = 1 }: { a?: number })`). A nested pattern's defaults apply the
  same way, also inside a tuple element (`const [i, { a = 5 }] = t` with
  `t: [number, { a?: number }]`).
- **Destructuring assignment** takes a value apart into places that already exist, as in JS:
  `[a, b] = [b, a]` swaps, and the targets may be variables, fields and elements
  (`[xs[i], xs[j]] = [xs[j], xs[i]]`, `[this.x, this.y] = [y, x]`, `({ w: box.width } = size)`),
  nested (`[p.x, { y: p.inner.y }] = src`). The value is evaluated first, then each target in
  order, left to right, each assigned before the next is evaluated (`[i, xs[i]] = [1, 5]`
  writes `xs[1]`). Defaults (`[a = 1, b = a] = xs`, `({ host, port = 80 } = opts)` with the
  shorthand `{ port = 80 }`, which is only allowed in such a target, TS1312) apply where the
  value is `undefined`, as in declarations, and each is evaluated when its turn comes, so it
  sees the targets assigned before it (`[a, b = a] = [5]` gives `5 5`). A last `...rest`
  takes the other elements or properties (`[first, ...others] = xs`, `({ id, ...rest } = p)`),
  a hole skips an element (`[a, , b] = xs`; a hole elsewhere, `[1, , 2]`, is not supported), and an object target needs parentheses as a
  statement (`({ a, b } = o);`), as in JS. `for...of` and `for...in` assign to such targets
  too, and to a variable or place declared elsewhere: `for (x of xs)`, `for ([k, v] of map)`,
  `for ({ name, port = 80 } of servers)`. The assignment has no value yet: `x = [a, b] = c`
  is an error (``a destructuring assignment cannot be used as a value yet``; assign `c` to
  `x` first).

  ```ts
  class Point { x = 1; y = 2; }
  function main() {
    let a = 1, b = 2;
    [a, b] = [b, a];
    const xs = [10, 20, 30];
    [xs[0], xs[2]] = [xs[2], xs[0]];
    const p = new Point();
    [p.x, p.y = p.x * 10] = [3];
    let host = "", port = 0;
    ({ host, port = 80 } = { host: "h" } as { host: string; port?: number });
    let first = 0, rest: number[] = [];
    [first, , ...rest] = xs;
    console.log(a, b, xs, p.x, p.y, host, port, first, rest); // 2 1 [ 30, 20, 10 ] 3 30 h 80 30 [ 10 ]
    let k = "", v = 0;
    for ([k, v] of new Map([["one", 1]])) console.log(k, v); // one 1
  }
  ```
- **Arrays** `T[]`: `length`, `xs[i]` (bounds-checked: panics
  `index out of bounds: the len is L but the index is I`; a read where a `T | undefined` is
  expected, `return xs[0]` from a function returning `T | undefined` or
  `const v: T | undefined = xs[i]`, and any read of an array whose element type allows
  `undefined`, gives `undefined` outside the array instead, as in JS), `push` (any number of items:
  `xs.push(a, b)`, `xs.push(...ys)`, all evaluated before the first is pushed, as in JS),
  `pop(): T | null`,
  `forEach map filter reduce find findIndex some every indexOf lastIndexOf includes slice concat
  reverse isEmpty entries fill`, `join` and `toString()` (any elements, written as
  `${xs}` writes them), `sort()` on
  numbers, strings and `Comparable` elements, and `sort(cmp)` (stable, any element type, like
  JS's `Array.prototype.sort(compareFn)`). Callbacks get the element and its index, like JS
  (`xs.map((x, i) => …)`, the index a `number`, also when written `(x: number, i: number)`),
  and may take fewer parameters. A predicate (`filter`, `find`,
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
  `get(k): V | undefined` (the stored value itself, as in JS; `undefined` for a missing key), `has`, `delete`, `size`, `keys()`,
  `values()`, `entries()`, `for (const [k, v] of m)`, plus single-lookup updates: `upsert(k, init, (v) => v + 1)`,
  `update(k, (v) => { v.push(x); }): bool` (the callback gets the stored value itself) and
  `getOrInsert(k, () => v)`. Keys compare as in JS: numbers, `bool` and `string` by value,
  objects (class instances, object types, arrays, maps, records, interface and function
  values) by identity, as `===` compares them, so two equal object literals are two keys and a
  key changed after insertion keeps its entry; tuples compare part by part. Structs are Velt
  value types: their keys compare and hash by content, field by field, so an equal struct finds
  the entry. Float keys compare
  like JS's (SameValueZero: `0` and `-0` are one key, `NaN` finds itself)
  ([Map](../std/prelude.md#map)). Iteration follows insertion order, like JS. A value type may
  include `null` (`Map<string, number | null>`, `Record<string, T | null>`): an entry holding
  `null` is present like any other (`has` is true, it is counted, iterated and printed), and
  `get` gives `null` for it and `undefined` for a missing key, as in JS. If
  such an entry holding `null` has a content key that changed after insertion, then while the
  map has deleted entries not yet compacted away, `forEach`, `for...of` and `console.log` skip
  it (`keys()`, `values()` and `entries()` list it).
- **`Record<K, V>`**: a dictionary written with object syntax, like TypeScript's `Record`.
  `K` is `string`, a union of string literal types, or a string enum; any other key type is
  an error (use a `Map`), also when a generic function or class gets it as a type argument.
  With `string` keys a record is *open*: `r[k]` and `r.name` are `V | undefined`, `r[k] = v`
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
  constant (a module `let` is read when the literal is built), or one that spreads a variable `r` of type `Record<string, V>`, is a
  `Record<string, V>` also where no record is expected (spreading a record into an object of a
  fixed shape is an error: its keys are known only at run time). A string or integer
  literal key in brackets is the plain key (`{ ["a"]: 1 }` is `{ a: 1 }`), as is a numeric
  property name, which is the number as `String` spells it (`{ 7: v }`, `{ 0x10: v }`,
  `{ 1.50: v }` and `{ 1e3: v }` have the keys `"7"`, `"16"`, `"1.5"` and `"1000"`, and
  `{ 9007199254740993: v }` the key `"9007199254740992"`, the nearest double). A computed key
  is a `string`, or a number converted to its string as in JavaScript (`{ [i + 0.5]: v }` with
  `i = 3` has the key `"3.5"`); a symbol made at run time is an error, and so is a key of any
  other type (`{ [true]: v }`, TypeScript's TS2464). In generic code, where the key type is a type
  parameter `K`, reads are `V | undefined` and the record may be closed, so it cannot start empty
  (only a literal with a spread builds one) and `delete` is not allowed. A record has no
  methods of its own and is not iterable: `Object.keys(r)` (a `string[]`), `Object.values(r)`
  and `Object.entries(r)` return arrays in JavaScript's order: keys that are array indices
  (`"0"` to `"4294967294"`) first, ascending, then the others in insertion order (`for (const
  [k, v] of Object.entries(r))`). `console.log` and `JSON.stringify` use the same order. Given an object literal, `Object.values` and `Object.entries` read it
  as a `Record<string, V>`, so its values need one type. `Object.keys` accepts any object, as
  in TypeScript: an object literal or object type (`Object.keys({ a: 1, b: "x" })` is `["a",
  "b"]`), a struct, or a class instance, whose fields it lists in declaration order (base class
  fields first, `private` ones too; not ES private `#x` fields, static fields or methods). A struct's optional field is
  listed only when it is present. On a class with subclasses it lists the fields of the
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
  there gives `undefined`, as in JavaScript, so a read is `V | undefined` (TypeScript types it
  `V`): `d.k === undefined` is `true` and `typeof d.k` is `"undefined"` for a missing key, and
  `d[k] ?? 0`, `d[k] || 0` and `d[k]?.push(x)` work as in JavaScript. The value type may include
  `null` (`{ [k: string]: number | null }`): a key holding `null` is present (listed by
  `Object.keys`, written by `JSON.stringify`) and reads `null`, a missing key `undefined`. As for `readonly T[]`, a `readonly`
  index signature is not checked for writes yet. Named members next to an index signature of
  `any` or `unknown` values (`interface Props { label: string; children?: string; [key: string]:
  any }`) make the type that record too, as every member's type is one the values have: a named
  member is a key like any other (written, deleted, listed and spread in insertion order). A
  required named member reads as its declared type (`props.label.toUpperCase()`,
  `props["label"]`, `props.n += 1`), checked at run time since an index write may store another
  type; an optional one reads as `any` (or
  `unknown`) for now (#930). A literal's value for a required named member, and a value written
  to one (`props.items = []`, `props["items"] = []`), is checked against its declared type, so
  `{ items: [] }` holds a `number[]` for `items: number[]` and a callback's parameters get
  their types (`{ onClick: (e) => ... }`). A value with more fields than the member's type is
  converted by copying them, as anywhere an object type is expected, and printing or
  serializing the record is then an error (Node would show the original's fields). A key holding a function is called as a
  method (`props.onClick()`, also on any `Record<string, any>`). A spread of a record that may be
  `null` or `undefined` (`{ ...props }` with `props: Props | null`) is a record too, and copies
  nothing when it is. A spread of a record of another value type into a record literal
  (`{ ...a, ...b }` of a `Record<string, string>` where a `Record<string, any>` is expected),
  or of a record keyed by string literals into one keyed by `string` (`Record<"a" | "b", V>`),
  converts each key and value into the new record, in the spread record's key order; a value that
  doesn't convert is an error. Not supported yet (each is an error): named members next to an index
  signature of other values (`{ name?: string; [key: string]: string | undefined }`, which needs
  `undefined`, #930), a `number` or `symbol` key, two index signatures in one type,
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

### `Object` built-ins

JavaScript's `Object` functions that work on Velt's objects behave as in Node. Each one is
compiled against the static types of its arguments; what a fixed object layout cannot do as Node
does is an error saying it is not supported yet, never a quiet difference.

- `Object.is(a, b)` is JavaScript's SameValue: `===`, except that `NaN` is itself and `0` is not
  `-0`, in generic code too (`Object.is(oldValue, newValue)` with `T` a number compares numbers
  that way). Values of unrelated types are never the same.
- `Array.isArray(x)` tests for the array members of `x`'s type and narrows `x` like `typeof`
  (`Array.isArray(x) ? x.length : x` on a `number | number[]`); a tuple is an array. An `any`
  or `unknown` value is tested at run time; a type parameter's value is not supported yet. On a `JsonValue` it is
  `x.isArray()`.
- `Object.assign(target, ...sources)` writes each source's own fields to `target`, in order
  (through its setters, as JavaScript's assignment does; an absent optional field is not copied,
  and one holding `undefined` where the target can't hold it is a `TypeError`)
  and returns `target` itself, copying enumerable symbol-keyed fields too (a brand
  `defineProperty` made with `enumerable: true`, below). A `Record` target takes any keys; an
  object, interface or class target only fields its type declares (adding one is not supported
  yet). `Object.assign(fn, { … })` onto a
  function makes a [function with properties](functions.md#functions-with-properties).
  Assigning from a value that may hold another class is not supported yet.
- `Object.freeze({ ... })` of a new object literal returns it with every field readonly, so
  writing one is a compile error, as in TypeScript. Node throws a `TypeError` when a frozen
  object's field is written through any reference, so a frozen object converts only to types
  whose fields are all readonly (`Readonly<T>`); passing it where a field could be written is not
  supported yet, and so is freezing anything but a new literal.
- `Object.fromEntries(pairs)` builds a `Record<string, V>` from `[key, value]` pairs: an array,
  a `Map` or any iterable.
- `Object.hasOwn(o, k)` and `Object.prototype.hasOwnProperty.call(o, k)` test `o`'s own keys: a
  record's entries, or the keys `Object.keys(o)` lists (methods and accessors are on the
  prototype, so they are not own keys).
- `Object.prototype.toString.call(v)` gives `"[object Array]"`, `"[object Map]"`,
  `"[object Null]"` and so on, for each type `v` may have.
- `Object.getPrototypeOf(v)` returns an `ObjectPrototype`, which `===` compares with
  `Object.prototype`, `C.prototype` for a class `C`, `Array.prototype`, `Map.prototype`,
  `Set.prototype`, `WeakMap.prototype`, `WeakSet.prototype`, `Promise.prototype` and `null`. A
  value of a class with subclasses gives the prototype of the class it holds. Prototypes print,
  convert to strings and to JSON as in Node; reading their members (`Object.prototype.toString`
  as a value) is not supported yet, and neither are the prototypes of other built-in values
  (`Date.prototype`, `String.prototype`) or of classes extending them.
- `Object.create(null)` makes an empty `Record<string, V>` without a prototype, where one is
  expected (`const d: Record<string, number> = Object.create(null)`): `"toString" in d` is
  `false`, `Object.getPrototypeOf(d)` is `null`, and it prints as `[Object: null prototype] {}`.
  `{ ...d }` copies it into an ordinary object. `Object.create` with another prototype, and
  `structuredClone` of a record in a program that calls `Object.create(null)`, are not
  supported yet.

- `Object.defineProperty(o, KEY, { value })` with a symbol constant `KEY` adds a brand: the
  compiler adds the optional field `[KEY]?: T` to the interface or object type `o` is declared
  with (`const cell: Cell = { ... }`, `{ ... } as Cell`, or a parameter `cell: Cell`; `T` is the
  type of the literal value),
  so `KEY in v` tests any `Cell` for it, as Node finds the property on the objects that have it.
  Unless the descriptor says `enumerable: true`, it is hidden from `console.log` and spreads, as a
  non-enumerable property is; a property with that symbol written directly anywhere (`{ [KEY]: v
  }`, `o[KEY] = v`) is an error, since it would be enumerable, or written where Node throws,
  unless the descriptors say `enumerable: true, writable: true`.
  Defining a brand again with another value throws Node's `TypeError: Cannot redefine property:
  Symbol(k)` (values compared as `Object.is` does) unless the descriptors say `writable: true` or
  `configurable: true`; Velt keeps these attributes per symbol, so every call with one key gives
  the same ones. With a string key naming a declared field and only a `value`, it assigns the
  field. The object is written in place and is the result (`Object.defineProperty(o, ...) === o`).
  A brand on a class instance (whose declared field is an own property from construction),
  other attributes and keys a type doesn't declare are not supported yet, and neither is
  `Object.defineProperties`.
- `Object.defineProperty(o, "k", { get, set })`, as a statement, on a `const o: I = { … }` of
  the same function whose literal has the accessor `get k() { … }` (and `set k(v) { … }` for a
  setter), with `I` an interface that declares it ([object literals implementing
  interfaces](#object-literals-implementing-interfaces)), replaces the literal's getter or setter,
  as in Node: one the descriptor leaves out stays, and after `configurable: false` defining it
  again throws `TypeError: Cannot redefine property: k`. A shorthand descriptor (`{ get }`) is
  `{ get: get }`. `enumerable` changes nothing Velt shows, since an interface value is not
  listed by key. Not supported yet: accessors defined on other objects (a parameter, a class
  instance, a loop variable named like the literal), in a module that binds the name `Object`
  anywhere (its `Object.defineProperty` may be its own), with `this` inside them (#941), that may
  throw (until #1326), that are `undefined` (the property would read `undefined`), and a
  function not written in the call where the property may already be defined with
  `configurable: false` (Node compares the functions).

```ts
interface Counter {
  get n(): number;
  set n(v: number);
}

function counter(log: boolean): Counter {
  let n = 0;
  const c: Counter = {
    get n() {
      return n;
    },
    set n(v) {
      n = v;
    },
  };
  if (log) {
    Object.defineProperty(c, "n", {
      set(v: number) {
        console.log("n =", v);
        n = v;
      },
    });
  }
  return c;
}

const c = counter(true);
c.n = 2; // n = 2
console.log(c.n); // 2
```

The other `Object` functions (`seal`, `isFrozen`, `setPrototypeOf`, ...) are not supported yet.

`typeof` of a global the program doesn't declare is what Node 22 gives in an ES module:
`"undefined"` for the browser's globals (`window`, `document`, `HTMLElement`, ...), other
runtimes' (`Deno`, `Bun`) and CommonJS's (`require`, `module`, `exports`, `__dirname`,
`__filename`), `"object"` or `"function"` for Node's (`globalThis`, `global`, `navigator`, `URL`, `Intl`,
`BigInt`, `AbortController`, `Headers`, `atob`, ...), and `"function"` for
a class's or a function's name, so `typeof window === "undefined"` tells the server from the
browser. Reading or calling one of the absent globals throws Node's `ReferenceError` (`window
is not defined`), so code that only runs elsewhere still compiles.

```ts
class Point {
  x = 0;
  y = 0;
}

function describe(v: number | number[]): string {
  return Array.isArray(v) ? `${v.length} numbers` : `${v}`;
}

const p = Object.assign(new Point(), { x: 3 });
const inert = Object.freeze({ state: "idle" });
const counts: Record<string, number> = Object.create(null);
counts.a = 1;
console.log(Object.is(NaN, NaN), Object.is(0, -0)); // true false
console.log(describe([1, 2]), describe(5)); // 2 numbers 5
console.log(p, inert.state); // Point { x: 3, y: 0 } idle
console.log(Object.getPrototypeOf(p) === Point.prototype); // true
console.log(Object.fromEntries([["a", 1], ["b", 2]])); // { a: 1, b: 2 }
console.log(counts, "toString" in counts); // [Object: null prototype] { a: 1 } false
console.log(Object.prototype.toString.call([1])); // [object Array]
```

#### `globalThis`

`globalThis` is a bag of typed properties, as libraries use it to share one value between
modules or copies of themselves: `(globalThis as { K?: T }).K` reads and writes a variable for
the whole process, which starts out `undefined`, as an unset property of Node's `globalThis`
does. The key may also be a string constant (`(globalThis as Seam)[KEY]`), the object a constant
that stands for `globalThis` (`const g = globalThis as Seam;`), and
`Object.defineProperty(globalThis, "K", { value, writable: true, configurable: true })`
assigns it. The property's type is `T["K"] | undefined` with `T` the asserted type; the
assertions of one key must all give it the same type, and a site without one (`globalThis.K`,
typed for tsc by a `declare global { var K: T }`, or asserted `any`) takes the type the others
give. A property no site asserts a type for is an error. A Node global Velt has is that global
(`globalThis.console.log(…)` is `console.log(…)`).

```ts
interface Hook {
  events: string[];
}
type Seam = { __HOOK__?: Hook };

function emit(e: string): void {
  (globalThis as Seam).__HOOK__?.events.push(e);
}

function install(): void {
  const g = globalThis as Seam;
  g.__HOOK__ ??= { events: [] };
}

emit("lost");
install();
emit("kept");
console.log((globalThis as Seam).__HOOK__?.events); // [ 'kept' ]
```

Not supported yet: `globalThis` as a value of its own (`console.log(globalThis)`,
`Object.keys(globalThis)`), symbol keys, keys known only at run time, `defineProperty` on it
without a `value`, neither writable nor configurable, without `writable: true` on a property
the program also assigns (Node throws there), or with other `writable` and `configurable` than
another call with the same key (an attribute a call leaves out is `false` on the property it
makes, so whether Node throws depends on which runs first), and using the result of
`defineProperty` on it.
`enumerable` changes nothing Velt shows, since the keys of `globalThis` are not listed.

### The `in` operator

`"key" in o` tests whether the object `o` has the member `key`, as in JavaScript. Velt objects
have fixed fields, so a literal key is answered from `o`'s type while compiling: a field, method
or accessor its object type, class (or a base class) or interface declares is always there, and
one it doesn't declare is not. A setter makes its name a member (`"w" in o` for `set w(v)`);
static members are the class's, not its instances', and a `#private` name is never a string key
(`"#p" in o` is `false`). An optional field of an object type or interface (`b?: T`) is tested
at run time: it is there once the object has it, also holding `undefined` or `null`
(`"b" in { b: undefined }` is `true`, as in Node). A class's optional field is always there
(TypeScript-compiled classes define it). A value whose class is known only at run time is
tested for it: a `Base` holding a `Sub` has the members `Sub` declares, and an interface value
has those of the class it holds, an optional field the interface declares included. The members every object inherits from `Object.prototype`
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
field keeps "absent" apart from a present `undefined`, also one of a dynamic type (`b?: any`,
`b?: unknown`): `"b" in { b: undefined }` is `true` and `"b" in {}` is `false`, and
`console.log`, `Object.keys`, spreads and `for...in` list such a key while it is present, as in
Node (`JSON.stringify` leaves out a field holding `undefined` either way). A primitive
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
- A method with a default body and an optional field may be left out (the field is `undefined`);
  every other member must be given, and a member the interface doesn't declare is an error.
- The value is an interface value: its members are read and called through the interface.
- Such a literal may be a module constant, exported or not (`export const upper: Handler = {
  … }`). Like every module constant it is evaluated at each use, and each use passed on is a
  new value, as for a function-valued constant
  ([Variables](variables.md#module-constants-initialized-by-a-call)), unless the program compares
  it by identity (`===`): it is then [module state](variables.md#module-state), one value as in
  Node.

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

- The methods use the variables around them, as arrows do, and `this`, the interface value
  they are called on ([`this` in object-literal methods](#this-in-object-literal-methods)).
- A generic interface method (`map<R>(f: (x: T) => R): R`) cannot be a function value: declare a
  class that implements the interface.
- A method that throws must be allowed to by the interface method's function type, as for any
  function value: an optional method (`m?(x: T): R`) is a field of function type, which
  doesn't allow throwing unless it says `throws`.
- `console.log` and `JSON.stringify` of such a value are not supported yet (as for other
  interface values).

### `this` in object-literal methods

In a method, getter or setter of an object literal, `this` is the object the method is called
on, as in TypeScript, where Velt knows the literal's type:

- where an interface is expected ([above](#object-literals-implementing-interfaces)), `this` is
  the interface value: its members are read, written and called through the interface;
- in a literal with no expected type (`const o = { … }`, or one returned from a function
  without a result type), `this` has the literal's own type, which TypeScript shows as
  `{ count: number; inc(): void }`. Such a literal, and one with getters or setters, is a value
  of an interface Velt makes from its members.

```ts
const counter = {
  count: 0,
  step: 2,
  inc() {
    this.count += this.step;
    return this;
  },
  get double() {
    return this.count * 2;
  },
  set double(v) {
    this.count = v / 2;
  },
};

counter.inc().inc();
console.log(counter.count, counter.double); // 4 8
counter.double = 20;
console.log(counter.count); // 10

interface Plugin {
  name: string;
  installs: number;
  install(app: string[]): void;
}

const router: Plugin = {
  name: "router",
  installs: 0,
  install(app) {
    this.installs++;
    app.push(this.name);
  },
};
const app: string[] = [];
router.install(app);
console.log(app, router.installs); // [ 'router' ] 1
```

- Arrows inside a method see its `this`, as in TS; an arrow property of the literal
  (`{ f: () => this.x }`) has the `this` around the literal, as it does in TS. A literal nested
  in a method has its own `this`. Generator and async methods take `this` too, and so does a
  function expression that is a property's value (`{ f: function () { return this.x; } }`).
- In a literal with no expected type, a method using `this` gives its parameter types
  (`add(n: number) { … }`), as TS's `--noImplicitAny` requires. Its result type comes from its
  body, also when it calls other methods through `this`; a method whose result depends on
  itself (`fact(n: number): number { … this.fact(n - 1) … }`) writes it, as TS requires
  (TS7023). A setter's parameter without a type takes its getter's.
- The object is passed to the method by the call (`o.m()`), so the object and its methods
  form no reference cycle. Such a method read as a value (`const f = o.m`, `xs.map(o.m)`) is an
  error in a literal with no expected type: Node would call it without its object (`this`
  `undefined`, a `TypeError` once it reads a member); call it on its object, or wrap it in an
  arrow (`(x) => o.m(x)`). A method of an interface value read as a value stays bound to the
  object it was read from ([methods as values](functions.md#arrow-functions-and-function-types)).
- A method read off another object as a property's value (`{ n: 2, m: c.m }`) is an error when
  its body may use `this`: Node calls it with the literal as `this` (`o.m()`), while the value
  read keeps the object it was read from. Write a method that calls it (`m() { return c.m(); }`).
  A method that doesn't use `this` may be such a value.
- `Object.keys` and `for…in` list a getter and setter pair once, under its name, as Node does:
  `Object.keys({ n: 1, get d() { … }, set d(v) { … } })` is `[ 'n', 'd' ]`.
- A getter fills the slot of an interface field when the literal's value is converted to an
  interface with methods (`o as V`, `const v: V = o` with
  `interface V { readonly value: number; show(): string }`), as in TypeScript; the getter runs
  on each read. Writing the field through the interface calls the literal's setter; without one
  it is an error (Node throws a `TypeError`). An interface with only fields is an object type,
  which such a value does not convert to yet.
- Not supported yet: `this` in a method of a literal whose expected type is an object type
  (`{ x: number; norm(): number }`, whose fields hold plain functions) or `any`; declare an
  interface instead. Calling such a method with another `this` (`.call`, `.apply`, `.bind`) is
  not supported either. Object spread (`{ ...o }`) of such a value, `console.log` and
  `JSON.stringify` of it are not supported yet, as for other interface values.

## bigint

TypeScript's `bigint` is a type name in Velt, but there are no `BigInt` values yet (no `10n`
literals, no `BigInt()`; arbitrary-precision integers are [`velt:bigint`](../std/bigint.md)).
Until there are, `bigint` is `never`: the type no value has. So types written for TypeScript
that mention it work for every value a Velt program holds: a union naming it is its other
members, a conditional type testing it never takes that branch, and a type bound naming it
still chooses overloads by its other members, as TypeScript does for those values.

```ts
type Primitive = string | number | boolean | bigint | null;
type Widen<T> = T extends number ? number : T extends bigint ? bigint : T;

function wrap<T extends Primitive>(target: T): { value: T };
function wrap<T extends object>(target: T): T;
function wrap<T>(target: T): { value: T } | T {
  return String(target) === "[object Object]" ? target : { value: target };
}

function main(): void {
  const state = wrap({ st: "idle" }); // the object signature
  state.st = "ready";
  const n: Widen<5> = 7;
  console.log(state.st, n); // ready 7
}
```

`typeof x === "bigint"` and `case "bigint":` in a `switch (typeof x)` are always false, and a
test the remaining type always passes leaves nothing behind (`if (typeof x === "string") return;`
on a `string | bigint`, then `x` is `never`). A value given where a `bigint` is expected is an
error, as in TypeScript; the message names the type `never`. So is an `any` or `unknown` given
there (`const b: bigint = a`, `a as bigint`), which TypeScript accepts: no value in a Velt program
is a `bigint`.

## any and unknown

`any` and `unknown` hold a value of any type together with its type, like C#'s `dynamic`. Every
value converts to either. An `any` converts back to every type, and the value is checked where it
enters typed code: a value of another type stops the program with a `TypeError` (Node would
keep the wrong value). An `unknown` must be narrowed first (`typeof`, `instanceof`), or
converted with `as T`, which is checked the same way. Both are the same value at run time, so
a container of one is a container of the other: a `Record<string, any>` is a
`Record<string, unknown>` (and back), and so are `any[]` and `unknown[]`, `Map<string, any>` and
`Map<string, unknown>`, `Set`s, nullables and function types that differ only there. It is the
same object seen through the other type, as in TypeScript: a write through one is seen through
the other, and `===` compares them as one value. A tuple (`[any, number]` as
`[unknown, number]`) converts too, but is copied, as every tuple is when assigned
([Values and references](memory.md#values-and-references)).

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
`typeof` test for `number`, `string`, `boolean` and `symbol` (`k as string | symbol`),
`instanceof` for a class, `===` for a literal, `Array.isArray` for the union's one array member;
a value that passes none of them converts to the union's one interface or object type member
when it has one, checked as `x as VNode` is: `VNode | string | number` from an `any`),
`typeof` (narrowing to `number`, `string` and `boolean`), `instanceof`, truthiness,
`===`, `!==`, `==` and `!=` (JavaScript's loose equality: `null == undefined`, `"1" == 1`) on any
operand whose type is `any` or `unknown` (a variable, a field, an element, a call), and
`console.log`. An object is itself in an `any` whatever its static type was when it was
converted (a class instance through its base class, an interface value, a function converted
twice), and one object converted twice is one value (`u1 === u2`, one `Set<unknown>` entry). As in TypeScript, a union with `any` is `any` (`any | null`, `any | undefined`) and one
with `unknown` is `unknown`: the `null` or `undefined` is held by the value itself, so
`m.get(k)` of a `Map<string, any>`, `r[k]` of a `Record<string, any>` and `find` on an `any[]`
give an `any` that is `undefined` when nothing is there, and `null` when a `null` was stored;
a left-out optional field `q?: any` holds `undefined`. A value whose type is a type parameter
converts to `any` and `unknown` too (`const a: any = t` with `t: T`, `t as unknown as X`): the
value of the instantiation goes in as above, so a `T` that is `number | null` holding `null`
gives the dynamic `null`, a union the member it holds and a literal type its value (an
instantiation with an enum stops with a `TypeError` for now, #1241). An enum value is its number or string
in an `any` (`typeof` is `"number"` or `"string"`), and converts back to the member with that
value. Generic code compares an `any` as `===` does (`indexOf`, `includes`, a generic
`a === b`), and `Set<any>` and `Map<any, V>` keys compare by value as in JavaScript (`NaN` is
itself, `-0` is `0`), objects by identity.
Differences from Node: `null` and `undefined` converted to `T | null` both give `null`
(a `T | null` has no `undefined`; convert to `T | undefined` to keep it); `==` between an
object and a boolean, number or string stops with a `TypeError` (JavaScript converts the object
with `ToPrimitive`: `[] == 0` is `true`); an instance of a subclass of a generic class does not
convert to that class (`Sub extends Box<number>` to `Box<number>`: a `TypeError`); and a
function prints as `[Function (anonymous)]` (Node prints its name, `[Function: f]`).

An object of another type converted to an object type (`a as Item` where `a` holds a class
instance with `Item`'s fields, or an object literal with more fields than `Item`) is, in a program
that makes proxies (`new Proxy` anywhere in its own code, as a reactive framework does), a *view*
of that object (#1485). Nothing is copied: each field is read from the object when it is read,
and checked then as the field's type, so a field the object lacks stops with a `TypeError` at
that read (Node reads `undefined`). A write goes to the object for the fields the object has;
writing an optional field the object lacks (`item.note = "hi"` on a class instance without
`note`) stops with the `TypeError` of a fixed-shape object (#924), where Node adds the property.
Converting the object again while a view of it lives gives that view (an object has one view at
a time, whatever object type it is converted to), so `===`, `Map`, `Set` and `indexOf` see one
value. Back in an `any` (and for `WeakMap`, `WeakSet` and `WeakRef`, `in`, `Object.keys`,
`for...in` and `JSON.stringify`) the view is the object itself: a `WeakMap` entry lives as long
as the object, and `deref()` gives its view again. Destructuring (`const { type, props } = vnode`, also in parameters and `for...of`),
spreads (`{ ...item }`) and keyed reads read the object's fields at that moment. A spread or an
object rest pattern (`const { name, ...rest } = item`) copies the fields of the view's type only
(`{ ...item }` of a `Product` viewed as `Item` has `name` and `price`, not `stock`; Node keeps
every own property), since an object type has a fixed shape; `JSON.stringify(item)` shows all
of the object's members. A view is copied like the object it views: `item.clone()` is a view of
a copy of the object, and a view sent to another thread (`spawn`, a channel, `shared(...)`, a
`Mutex`'s value, a task's result) while the object is still used here is a view of the copy
there ([Async](async.md)), so a write there does not reach the object here, and converting the
object here again gives a view of its own. A union with
one object type member (`VNode | string | null`) converts an object that is no `VNode` the same
way. In a program without proxies, and into a class or an interface with methods, an object of
another type stays a `TypeError`.

```ts
type Item = { name: string; price: number };

class Product {
  name = "pen";
  price = 2;
  stock = 10;
}

function main(): void {
  const tracked = new Proxy({ hits: 0 }, {}); // the program makes proxies
  const p = new Product();
  const a: any = p;
  const item = a as Item; // a view of `p`
  item.price = 3;
  console.log(p.price, item.name, tracked.hits); // 3 pen 0
  const back: any = item;
  console.log(back === a, item === (a as Item)); // true true
}
```

Members of an `any` are read and called as in JavaScript: `x.f`, `x["f"]`, `x[k]` with a
`string`, `number` or `any` key (any expression: `x[key()]`, `x[i + 1]`), `x.m(a, b)` (a method,
or a field holding a function, typed or `any`) and `x(a, b)` when `x` holds a function. The member
is found at run time in the value the `any` holds: a field, getter or method of a class
instance (a subclass's too when it was converted through its base class), an object type's
field, `length` and elements of an array, `length` of a string, and the members of the standard
library's classes (`size`, `get`, `set` and `has` of a `Map`). The result is an `any`; each
argument is converted to the parameter's type with the same check as above, a left-out one is
`undefined`, and extra ones are ignored. A spread argument (`f(...args)`, `o.m(a, ...rest)`)
passes the elements of an array, each converted the same way.

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
members whose type is an enum. A member of a literal type (`kind: "ok"`), a union or an
interface reads as the value it holds.

Members of an `any` are written as in JavaScript too: `x.f = v`, `x["f"] = v` and `x[k] = v`
with a `string`, `number` or `any` key. The value is converted to the member's type with the
same check as above (a value of another type stops with its `TypeError`, and the member keeps
its old value) and stored in the object itself, so every other reference to it sees the write;
a setter is called with it. A `Record<string, V>` takes any key, adding the ones it lacks, as
in Node; an array takes its elements inside its length, an element at its length (it grows by
one, as `a[a.length] = v` does in Node) and a shorter `length`, which drops the elements past it
(a length that is not a whole number from 0 up is Node's `RangeError: Invalid array length`). The
other values have a fixed shape:
adding a property stops with `TypeError: Cannot add property k: an object of a fixed shape gets
no new properties through `any` (#924)` (Node adds it), writing a getter that has no setter with
Node's `TypeError: Cannot set property k of #<C> which has only a getter` (what strict code
gets; in sloppy code Node ignores the write), and a `null` or `undefined` receiver with Node's
`TypeError: Cannot set properties of undefined (setting 'k')`, after the value is evaluated.
Writing a `readonly` field, a method, an element further past the end of an array or a longer
`length` (Node leaves holes, which Velt's arrays don't have), or a member of a
string, number or function stops with a `TypeError` naming #924. `x.f ??= v`, `x.f ||= v` and
`x.f &&= v` read the member once and evaluate and write `v` only when the operator calls for it,
as in JavaScript; the expression is the member read or the value written. The other compound
assignments (`x.n += 1`) and `x.n++` are compile errors until the operators of `any` come.

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

`<`, `>`, `<=` and `>=` with an `any` operand compare as JavaScript does: two strings by their
UTF-16 code units, any other pair as numbers, each converted with `ToNumber` (`true` is 1,
`null` is 0, `undefined` and a string that is not a number are `NaN`, and every comparison with
`NaN` is `false`). Comparing a symbol stops with Node's `TypeError: Cannot convert a Symbol value
to a number`; comparing an object or a function (which JavaScript converts with `valueOf` /
`toString`) stops with a `TypeError` naming #924 for now.

```ts
function main(): void {
  const n: any = 5;
  const s: any = "10";
  const u: any = undefined;
  console.log(n < 7, s > 9, s < "9", n < u, n >= u); // true true true false false
}
```

`String(x)`, a template literal and a string `+` / `+=` write an `any`, `unknown` or `object`
by the value held, as JavaScript's `ToString(ToPrimitive(x))` does, not as `console.log` prints
it. `String(x)` and templates ask a class instance for its `[Symbol.toPrimitive]("string")`,
else its `toString()` (its own or a base class's, an `Error`'s), then its `valueOf()` when that
returns an object; `+` asks for `[Symbol.toPrimitive]("default")`, else its `valueOf()`, then
its `toString()` when that returns an object (each result tested when the program runs,
whatever its declared type). An object literal is written by its own `toString` and `valueOf`
members in the same order; another object as `Object.prototype.toString` does
(`[object Object]`, `[object Map]`). When every method returns an object, the program stops
with Node's `TypeError: Cannot convert object to primitive value`, and when one returns a
symbol with `TypeError: Cannot convert a Symbol value to a string`, reporting the line of the
`String`, template, `+` or `join`. An array (also one held in a typed `any[]`, `unknown[]`, `object[]` or a tuple
of them, and its `join()` and `toString()`, also `xs.join(sep)` of an `any` `xs` holding one) is
written as `Array.prototype.join` does: each
element as `String(x)` writes it, `null` and `undefined` elements and an array holding itself
as empty text (`[1, [2, 3], null]` is `1,2,3,`). In a template or a `+`, a symbol stops with
Node's `TypeError: Cannot convert a Symbol value to a string` (only `String(s)` writes it, as
`Symbol(desc)`); a function stops with a `TypeError` naming #924 (Node writes its source text),
and so does a proxy (Node reads its `toString` through the `get` trap). So is a value whose
type is a type parameter instantiated with `any`, `unknown` or `object`, or an array or tuple
of them (`String(v)` or `` `${v}` `` in `function show<T>(v: T)` called as `show<any>({ a: 1 })`
is `[object Object]`, as `show<unknown[]>([{ a: 1 }])`; a symbol in a template or a `+` stops
with the `TypeError`); the function's other instantiations write their own types' text.

```ts
class Celsius {
  valueOf(): number {
    return 21;
  }

  toString(): string {
    return "21 °C";
  }
}

function main(): void {
  const t: any = new Celsius();
  console.log(`${t}`, "" + t); // 21 °C 21
  const xs: any[] = [[1, 2], { a: 1 }, null];
  console.log(String(xs), xs.join(" | ")); // 1,2,[object Object], 1,2 | [object Object] |
}
```

`Object.is(a, b)` with an `any` or `unknown` operand is SameValue, as in JavaScript: `===`,
except that `NaN` is itself and `0` is not `-0`. `Number.isFinite(x)`, `Number.isNaN(x)`,
`Number.isInteger(x)` and `Number.isSafeInteger(x)` take an `any` or `unknown` as TypeScript's do,
and are `false` unless `x` holds a number.

`for (const x of v)` over an `any` holding an array visits its elements as `any`s, reading
the length before each step as JavaScript's array iterator does. Another value (a string, a
`Map`, an iterable object) stops with a `TypeError` naming #924 for now.

`Object.keys(x)` of an `any`, an `object` or a `Function` lists the own enumerable string keys
of the value held, in JavaScript's order (integer-like keys first, ascending, then the others in
order): the fields of a class instance (a subclass's too) or object (an optional field only while
present), a `Record`'s keys, an array's or a string's indexes as strings, and none for a number,
a boolean, a function, a `Map` or a `Set`. `null` and `undefined` stop with Node's `TypeError:
Cannot convert undefined or null to object`; a proxy runs its traps ("Proxy and Reflect"
below). `Object.keys` of an `unknown` is an error until it is narrowed, as in TypeScript. A spread
of an `any` or `object` into an object literal (`{ ...a, b: 1 }`) builds a `Record<string, any>`
with those keys and their values (`null` and `undefined` spread nothing):

```ts
function main(): void {
  const a: any = { x: 1, y: "two" };
  console.log(Object.keys(a)); // [ 'x', 'y' ]
  const merged = { ...a, z: true };
  console.log(merged); // { x: 1, y: 'two', z: true }
}
```

`Object.hasOwn(x, k)` and `Object.prototype.hasOwnProperty.call(x, k)` of an `any` or `object`
test the keys `Object.keys(x)` lists (and an array's or string's `length`); the key may be a
`string | symbol` (a proxy trap's `prop`), and a symbol is not among those keys.
`Object.getPrototypeOf(x)` gives the prototype of the class of the value held (`C.prototype`),
`Object.prototype` for an object or a record (`null` for one made by `Object.create(null)`) and
`Array.prototype` for an array, so `proto === Object.prototype || proto === null` tells a plain
object apart; a value whose prototype Velt doesn't name (a number, a function, a `Date`) stops with
a `TypeError` naming #924. `Object.prototype.toString.call(x)` gives the `[object Tag]` of the
value held. `k in Object.prototype` with a string known only at run time tests the members every
object inherits (`toString`, `constructor`, ...).

`JSON.stringify(x)` of an `any`, `unknown` or `object` (also inside a typed value, such as an
`unknown[]` or a `Record<string, any>`) writes the value held as Node does, with a class's
`toJSON()` and an interface value written as the object it holds: `undefined`, functions and symbols are left out as object members and written as
`null` in arrays, and an object that holds itself stops with `TypeError: Converting circular
structure to JSON`. Node returns `undefined` for such a value at the top; a Velt `string` cannot
hold it, so `JSON.stringify` of an `any` holding `undefined`, a function or a symbol stops with a
`TypeError` naming #924. A value with no JSON form here (an object with a function field, a
proxy) also stops with a `TypeError`.

An object literal converted to `any` or `unknown` (`const u: unknown = { a: null }`, also
`{ b: { c: null } }` and `{ a: [null, 1] }`) gives a `null` field TypeScript's type `null`: it
holds an `any` that is `null`. A `void` value or one of type `undefined` (a call of a function
returning it) converts to an `any` holding `undefined`.

**Planned** (#924): the other compound assignments and `++` on members, arithmetic operators,
`JSON.parse` into an `any`, `any` converted to interface types and the warning on a
written `any`; until then each is a compile error.

`key in x` on an `any` asks the value held: a class instance or object type has its fields,
methods and accessors and what every object has from `Object.prototype` (`toString`), a
`Record<string, V>` its keys and, unless `Object.create(null)` made it, what every object has,
an array its `length`. A `null` or `undefined` value stops with
Node's `TypeError: Cannot use 'in' operator to search for 'k' in undefined`. An optional field
of a nullable type (`a?: T | null`), which keeps "absent" apart, is there while present;
another optional field (which cannot tell "absent" from a present `undefined`) and an array
index stop with a `TypeError` naming #823 for now.

A symbol constant is a key on an `any` too: `x[KEY]`, `x[KEY] = v` and `KEY in x` (with
`const KEY = Symbol("k")`, or a well-known symbol such as `Symbol.iterator`) name the member
the symbol keys, as on a typed object. A key of type `string | symbol` (a proxy trap's `prop`)
reads, writes and tests the member a string names; a symbol known only at run time stops with
a `TypeError` naming #924 for now.

```ts
const BRAND = Symbol("brand");

function isBranded(v: any): boolean {
  return v !== null && typeof v === "object" && v[BRAND] === true;
}

function main(): void {
  const b = { [BRAND]: true, name: "b" };
  console.log(isBranded(b), isBranded({ name: "c" }), BRAND in (b as any)); // true false true
}
```

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
`const`, a second conversion to `Handler<any, any>`) wraps that same object. A field assigned
through the converted value (`h.name = "x"` on a `Handler`) is assigned on the original, its
value converted to the original's field type and checked as an argument is (a field of an
interface type at `unknown` arguments does not convert back, a compile error naming #1185).
The converted value and the original are one object to `===`, `instanceof`, `Set` and `Map`
keys, and `includes`: a `Handler<Date, number>` read back from a `Handler[]` is `===` to it,
`instanceof` tests the original's class, and a `Set<Handler>` holding one finds the other. The
same holds once either is an `any`, `unknown` or `object` (which holds the original itself), and
members read or written through such an `any` are the original's.

A checked conversion from `any` or `unknown` back to the interface (`v as Handler<any, any>`,
or at any other arguments, such as the original's own) takes the value the `any` holds through
its own implementation of the interface: an object literal's, a class's (also for an instance
of a subclass, through the nearest base class implementing it), or a generic class's at the
instance's type arguments, including instances only generic code makes (`new G<T>(x)` in a
`function mk<T>(x: T)`). At other arguments the result is wrapped as above, and it is the same
object as the original. A context stored in a `Map<string, unknown>` and read back as a
`Ctx<any, any>` works as in Node. A value implementing the interface at a promise argument
(`Ctx<Promise<number>>`) does not convert to `Ctx<any>`: a `TypeError`.

A field holding an array of the interface's own values (`kids: Tree<T>[]`), read through a
`Tree<number>` converted to a `Tree<any>`, is a new array of the same elements, each a
`Tree<any>`: reads see the original's elements and their changes. Once a program converts a
`Tree` to other type arguments this way, the field read through a `Tree<any>` (or through a
`Tree<T>` in generic code, which may be one) may only be used in place without changing it:
an element (read, or written through: `t.kids[0].value = 7` changes the original's element),
`.length`, or a method that leaves the array unchanged (`map`, `filter`, `slice`, `indexOf`,
`join`, …; a callback that replaces or removes an element of the original during the call may
still be given the element the field held when the method started), also behind `!` or `?.` (`t.kids?.length`,
`t.kids?.[0]`) or after a test of an optional field (`if (t.more) t.more[0]`), and a
condition or `null` test of it (`if (!t.kids)`, `t.kids && t.kids.length`). The same holds for a
method's result of such a type (`getKids(): Tree<T>[]`). Anything else is a compile error naming
#1185, as Node's result would differ: changing the array (`push`, `splice`, `sort`, an element
or `length` assigned, the field assigned), keeping it (`const k = t.kids`, destructuring,
spreading), passing it on, comparing it (`t.kids === t.kids` is `false` on a new array), or
iterating it with `for…of`, `entries()`, `keys()` or `values()` (Node also visits an element the
loop body adds to the original; an index loop reading `t.kids.length` and `t.kids[i]` each step
sees them). Change or keep the array through
the value's own type instead. An interface whose function types take or give such an array
(`withKids(f: (k: Tree<T>[]) => void)`, `kidsFn: () => Tree<T>[]`) does not convert to other
type arguments: a compile error naming #1185.

```ts
interface Tree<T> {
  value: T;
  kids: Tree<T>[];
  size(): number;
}

class Node1<T> implements Tree<T> {
  value: T;
  kids: Tree<T>[] = [];
  constructor(v: T) {
    this.value = v;
  }
  size(): number {
    return this.kids.length;
  }
}

function main(): void {
  const t = new Node1<number>(1);
  t.kids.push(new Node1<number>(2));
  const a = (t as unknown) as Tree<any>;
  console.log(a.kids.length, a.kids[0].value); // 1 2
  console.log(a.kids.map((k) => k.value).join(",")); // 2
  t.kids.push(new Node1<number>(3)); // `a.kids.push(…)` is the compile error
  console.log(a.kids.length); // 2
}
```

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

The handler's `ownKeys` trap lists the proxy's own keys and `getOwnPropertyDescriptor` describes one
of them, so a proxy may have keys only its handler knows (an empty target). As in Node,
`Object.keys(p)`, `Object.values(p)` and `Object.entries(p)` run `ownKeys`, then
`getOwnPropertyDescriptor` for each string key (and `get` for an enumerable one, for values and
entries); a spread `{ ...p }` does the same, key by key. `Object.hasOwn(p, k)` and
`Object.prototype.hasOwnProperty.call(p, k)` run `getOwnPropertyDescriptor`. `delete p.k` and
`delete p[k]` run the `deleteProperty` trap, and a trap that returns `false` throws Node's
`TypeError: 'deleteProperty' on proxy: trap returned falsish for property 'k'`. A trap the handler
leaves out forwards to the target: its keys are what `Object.keys` lists, each a data property
(writable, enumerable and configurable; an array's `length` is not enumerable and not configurable).
`Reflect.ownKeys(t)`, `Reflect.deleteProperty(t, k)`, `Reflect.getOwnPropertyDescriptor(t, k)` and
`Reflect.defineProperty(t, k, attributes)` are those operations as functions (a `PropertyDescriptor`
is `{ value?, writable?, get?, set?, enumerable?, configurable? }`), and a target that is not an
object stops with Node's `TypeError: Reflect.ownKeys called on non-object`. The checks Node makes on
what these traps answer apply, with Node's `TypeError`s: an `ownKeys` answer must be an array of
strings and symbols without duplicates that lists the target's keys that are not configurable, and a
`getOwnPropertyDescriptor`, `deleteProperty` or `defineProperty` answer must agree with the target's
own property (a property may be reported non-configurable only when the target's is). When the
target is itself a proxy, the target's traps run for these checks, in Node's order. A descriptor the
`getOwnPropertyDescriptor` trap answers is completed: an accessor (`get` / `set`) or a data
property, an attribute left out `false` (and a data property's `value` `undefined`). The handler's
`defineProperty` trap runs for `Reflect.defineProperty(p, k, attributes)`, and `Reflect.set(t, k, v,
p)` with a proxy `p` as the receiver runs `p`'s `getOwnPropertyDescriptor` and `defineProperty`
traps, as Node does. `Reflect.defineProperty` of a plain value writes the value of an existing
property; a new property, whose attributes left out are `false`, is a `TypeError` naming #823.
`delete x.k` through `any` and `Reflect.deleteProperty(x, k)` of a value that is not a proxy remove
a `Record`'s entry and make an object type's optional field absent; a key the value doesn't have as
its own (a class's methods are on its prototype) gives `true`, and an array's `length` gives `false`
(`delete a.length` throws Node's `TypeError: Cannot delete property 'length' of [object Array]`). A
field the value's type fixes and a class's field (an own property even when optional) can't be
removed: that is a `TypeError` naming #823. Deleting a string's `length` or one of its characters
throws Node's `TypeError: Cannot delete property '0' of [object String]`; another key of a string,
and any key of a number or a boolean, gives `true`.

A trap sees a symbol key as the symbol (`p[KEY]`, `KEY in p`, `delete p[KEY]`,
`Object.hasOwn(p, KEY)` with a symbol constant `KEY`), and `Reflect.get(t, KEY)` /
`Reflect.set` / `Reflect.has` of a plain value reach the member the symbol names. A handler
typed `any` (`new Proxy(t, handler as any)`) works too: its traps are called through `any`.
`JSON.stringify` of a proxy reads it through its traps in Node's order (`toJSON`, then the
keys, whether each is enumerable and their values; an array target's `length` and elements).
`Object.keys(s)` of a typed proxy `s` runs its `ownKeys` and `getOwnPropertyDescriptor` traps,
`for (k in s)` its `ownKeys` trap once and its `getOwnPropertyDescriptor` trap just before each
key's turn, `Object.hasOwn(s, k)` and `hasOwnProperty.call(s, k)` its
`getOwnPropertyDescriptor` trap for `k`, and `"k" in s` its `has` trap, as in Node, for a
proxy typed as a class, an interface or an object type (an operand such as a call,
`Object.keys(getProps())`, is evaluated once). `for (k in p)` of an `any` proxy `p` runs its
`ownKeys` trap once and its `getOwnPropertyDescriptor` trap just before each key's turn,
visiting the enumerable ones, so a key deleted during the loop is skipped, as Node does. A
`for...in` loop evaluates its object once and keeps it: assigning another value to the variable
or property in the body changes neither the keys visited nor the traps run. Whether the value is
a proxy is tested once, before the loop; a plain value's keys (also through `any`) are listed
when the loop begins, and one deleted during the loop is still visited.

`p.push(…)`, `p.pop()`, `p.shift()`, `p.unshift(…)`, `p.splice(…)`, `p.map(f)`, `p.filter(f)`,
`p.forEach(f)`, `p.some(f)`, `p.every(f)`, `p.find(f)`, `p.includes(x)`, `p.indexOf(x)`,
`p.join(sep)`, `p.slice(…)`, `p.reverse()` and `p.sort(compare)` on a proxy of an array read the
method through the `get` trap and run JavaScript's generic algorithm with the proxy as `this`, as
Node does (the callbacks get the element, its index and the proxy; a `thisArg` is not passed on yet;
`map`, `filter` and `slice` give a new plain array): every read of `length` or of an element, every
write (`set`), `in` test (`has`) and delete (`deleteProperty`) goes through the traps, in Node's
order, and a trap that refuses throws Node's `TypeError`. A trap that forwards with `Reflect`
reaches the array: `Reflect.has(a, "1")` tests an element, and `Reflect.get(a, "push")` is the
method, a function to `typeof` and `console.log` (`[Function: push]`; `Reflect.get(a,
"constructor")` is `[Function: Array]`), so a reactive `get` trap that wraps only objects passes it
through and a nested reactive array runs it. Each read gives a new one, so two reads are not `===`
as in Node, and called other than on a proxy it is a `TypeError` naming #924. While such an
algorithm runs, the holes its own trap calls make in its target behave as in Node: `pop`, `shift`
and `splice` delete the last elements (`Reflect.deleteProperty(t, k)`) before they write the shorter
`length`, and `unshift` and `splice` write an element past the end before they fill the gap. The
array keeps its length and its elements (Velt's arrays have no holes; a gap holds the value written
after it), and `Reflect.get`, `Reflect.has`, `Reflect.ownKeys`, `Reflect.getOwnPropertyDescriptor`,
`Object.hasOwn` and `hasOwnProperty.call` see those elements as absent until the algorithm fills
them or writes the shorter `length`. A trap that doesn't write it, so that holes would stay, is a
`TypeError` naming #823, and so is a hole in another array or one made in a callback the algorithm
calls (`forEach`'s, `sort`'s comparison). Behind a proxy of a proxy the holes are the array's at the
end of the chain. `map` and `slice` of an element that is absent (a `has` trap answering `false`, a
hole) are a `TypeError` naming #823 (Node keeps the hole in the new array); `filter`, `forEach`,
`some` and `every` skip it, as in Node. `Array.isArray` of a proxy of an array, or of a proxy of
one, is `true`, and `JSON.stringify` writes it as an array. Elsewhere, deleting an element that is
not past the end (`delete a[i]` through `any`, `Reflect.deleteProperty(a, i)`, the last element too)
and `Reflect.set(a, i, v)` further past the end are `TypeError`s naming #823 and #924. `const hasOwn
= Object.prototype.hasOwnProperty` reads a value whose `hasOwn.call(o, k)` is `Object.hasOwn(o, k)`.

The handler is a `ProxyHandler<T>`: its traps' parameters take their types from it, as in
TypeScript (`target: any`, `p: string | symbol`, `receiver: any`). `new Proxy(target, handler)`
is an `any`, which converts to the class or object type the context expects (`as State`, an
annotation, a return type, an argument). The typed value is the same proxy: reading `s.count`,
writing `s.count = 1`, `s.count += 1` and passing it on run the traps as before, `s === t` and
converting it back to `any` see one object, and `console.log(s)` prints the target. A field
read through the `get` trap is checked against the field's type when it arrives (a `TypeError`
on a mismatch), so a proxy may stand for a type whose fields its target doesn't have (an empty
target whose `get` trap answers every key). A method called through a typed proxy runs with
`this` the proxy, so its reads and writes of `this` run the traps too. When the handler has a
`get` trap, `s.m(a)` first reads `m` through it, before `a` is evaluated, as JavaScript does; a
trap that forwards with `Reflect.get(target, key)` or `Reflect.get(target, key, receiver)` gives
the target's method, which then runs with `this` the proxy. A typed proxy passed where an object
type with fewer fields is expected (a `State & { $set(v: State): void }` passed as a `State`)
stays the same proxy, so writes through the `State` run the `set` trap and reach the target.
(Where a copy of a plain object could be told from the original, such a conversion is a compile
error in a program without proxies; in one that creates proxies, a plain object there throws a
`TypeError` naming #823 instead of being copied.)
Calling a function-typed field of a typed proxy (`s.$set(v)`) reads it through the `get` trap
before the arguments, then calls the function the trap answered, whatever its own parameter
types (a closure taking `any`), and checks its result against the field's result type.
`s.count++` used as a
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
in a program that creates a proxy, so other programs and other types pay nothing. `Proxy`,
`Reflect`, `ProxyHandler` and `PropertyDescriptor` are globals that a program loads when one of
its modules names them, like `fetch` and `URL` ([the prelude](../std/prelude.md)): a program
that never names them doesn't compile their code, and its operations on `any` (a spread,
`Object.hasOwn`, `Object.values`, `Object.entries`, `delete`, `o[KEY]` with a symbol constant)
don't test for a proxy.

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

**Planned** (#823): a symbol known only at run time as the key of `o[k]` on an `any` (a trap
answering one through `Reflect` works); a spread of a typed proxy (a `TypeError`); a completed
descriptor's `value: undefined` (an optional `any` field cannot hold a present `undefined`,
#930); the other array methods through a proxy of an array (`lastIndexOf`, `findIndex`, `reduce`,
`concat`, ...), and `Array.prototype[m].apply(p, arguments)` (#1381);
an `ownKeys` trap answering an array-like object that is not an array; deleting a fixed field,
an array element other than the last or a symbol-keyed member through `any` from a value
that is not a proxy, and `Reflect.defineProperty` of a plain value with accessors or
attributes other than `true`; proxies typed
as an array or a `Record`, and `in` on a typed proxy; a function the `get` trap returns read
as a value from a function-typed field (`const f = s.$set`), or one it answers for a method name
of a class; an async method called through a proxy with a `get` trap; a method call through an
interface value holding a proxy whose handler has a `get` trap; a method read as a value through
`any` and called on its own; `Array.isArray` of an array proxy; proxies of functions (`apply`,
`construct`); `Reflect.get` / `Reflect.set` with a receiver
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
  called the same way (`plugin.setup(app)`). `f.call(thisArg, a, b)` and
  `f.apply(thisArg, [a, b])` are the call `f(a, b)` when `thisArg` is `null` or `undefined` (or
  left out: `f.call()`); `f` and `thisArg` are evaluated once, before the arguments. A Velt
  function value has no `this` to set, so another `thisArg` stops with a `TypeError` naming
  #926 (Node would call the function with it).
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
- `x.constructor.name` and `x.constructor?.name` of an `object`, `any` or `Function` give the
  name of the constructor of the value held, as in JavaScript: its class's (a subclass's when
  converted through its base), `"Object"` for an object literal or a record, `"Array"`,
  `"Map"`, `"Uint8Array"`, `"Number"` for a number. A function, or an instance of a standard
  library class that JavaScript has no constructor of the same name for, stops with a
  `TypeError` naming #926; `null` and `undefined` with Node's `TypeError: Cannot read
  properties of null (reading 'constructor')`.

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
members every object has (`constructor` as a value, `toString`, `hasOwnProperty`) and a function has
(`call` and `apply` with another `thisArg`, `bind`, `name`, `length`), and a type parameter without the `object` bound
converted with `as object`. Each is a compile error.
