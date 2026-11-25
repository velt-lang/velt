# The JSON engine

Status: partly implemented. Issue: [#1655](https://github.com/velt-lang/velt/issues/1655).

## Summary

Velt's JSON is built the way .NET's System.Text.Json (STJ) is built with its source generator:
there is no reflection at run time. `JSON.parse<T>` and `JSON.stringify<T>` compile to one
decoder or writer per type, generated at compile time by std's source generators, on top of a
pull reader and a writer that are themselves Velt code. `JSON.parseValue` reads documents of
unknown shape into a flat tape.

The engine aims to be the fastest JSON in any language for typed data and faster than Node for
dynamic data, with STJ's features: naming policies, renamed, ignored and required fields,
polymorphic discriminators, converters, extension data, parsing from bytes, writing to a sink,
streaming and schema export. The engine, the per-type code and the dynamic value exist; the
features that change the API are described under "Not yet implemented".

## Motivation

The per-type code calls the reader for every token. When the reader sits behind opaque calls
into a runtime written in another language, LLVM can inline none of it: each key, each number
and each string costs a call it cannot see through. A reader written in Velt is compiled with the
generated decoder, so it is inlined, specialized per type and register-allocated as one unit,
and every improvement to the compiler speeds up JSON too.

Typed JSON in real programs also needs more than the shape of the type: wire names that differ
from field names (`created_at` for `createdAt`), fields that are not written when null, required
keys, tagged unions of classes, and custom encodings. STJ covers these with attributes and
options; Velt covers them with compile-time decorators, so they cost nothing at run time.

## Design

### Rule: the engine is written in Velt

The engine is Velt code in `std/`, built only on APIs every program has (`u8[]`, strings,
`velt/simd`, integer types, classes and structs): the scanner, the pull reader, number parsing,
string escaping, the tape and the writer (what remains in the runtime is listed under
"Compilation"). No new logic goes into Rust in
`velt_rt` or `velt_text`, and no Rust crate is used. The reasons:

- **It proves the claim.** If `velt/encoding/json` is Velt and beats serde_json, the language is
  fast. Rust behind an extern call would only show that Rust is fast.
- **The code is faster.** A reader in Velt is inlined into the generated decoders; a runtime
  reader is a call per token.
- **Gaps are fixed in the compiler.** Where Velt is slower than the Rust it replaces (bounds
  checks, SIMD lowering, inlining, layout), the fix goes into the compiler or the language and
  speeds up every program. A Rust fast path would hide the gap.
- **One language and one ABI.** The same code runs on every target, WebAssembly included, with
  no runtime ABI to keep in step.

The same rule governs the SQLite engine ([sqlite-engine.md](sqlite-engine.md)).

### Typed parsing and writing

`JSON.parse<T>(text, options?)` calls `parseJsonTop<T>`, and `JSON.stringify<T>(value)` calls
`writeJsonTop<T>` (`std/prelude/json/json.velt`). Both are `declare generated` functions
(`std/encoding/json/generated.velt`): a generator in `std/encoding/json/generate/` writes one
Velt function per type at compile time ([source-generators.md](source-generators.md)). The
decoder for an object reads each key with the reader's `nextKey()`, compares its UTF-8 length
with each field's and then its bytes in place (`keyIs`, no string made), decodes the value into a
local and, at the end, checks that every required field was present. A failure carries its path
(`expected string at $.items[3].name`): each level it passes through puts its segment in front,
so paths cost nothing on success.

A key with an escape (`"name"`) is compared decoded, so it matches `name` as in Node. A
repeated key keeps its last value.

The generated decoders handle numbers (integers range-checked, the type named in the message;
`u64` over its whole range), booleans, strings, literal types, enums, options, arrays, tuples,
unions, `Map<string, V>`, `Record<K, V>`, `JsonValue`, and objects, structs and classes of those.
A type the generators cannot handle is a compile error at the call, listing what JSON supports.

### The dynamic value

`JSON.parseValue` writes a **tape** (`std/encoding/json/tape.velt`): two `u64` words per value
in one array. The first word holds the kind, flags (escaped string; repeated, reordered or
escaped keys) and, from bit 32, a string's UTF-16 length or a container's member count. The
second holds a number's bits, a string's byte range in the source (`start | end << 32`) or its
index among the decoded strings, or a container's first position in a second array that lists
each container's members together, so `at(i)` is O(1). Strings without escapes stay in the
source until read. Limits: 2^32 bytes of source and 2^32 members per container, both past the
largest string Velt allows.

**JavaScript order.** The tape keeps source order. An object whose keys are already in
JavaScript order (no index-like key, no repeated key) is read as it is. Any other object gets a
member list in JavaScript order (index keys first, ascending, then the rest; a repeated key at
its first place, with its last value), which `keys()`, `at(i)`, `len()` and `stringify()` read.
Objects with more than 16 keys get a hash index on their first lookup.

**Sharing.** A parsed tape is frozen: values of one document, sent to other tasks, share it
instead of copying it.

**Representation.** A `JsonValue` (`std/prelude/json/json_value.velt`) has two forms: a position
on a tape (what parsing makes, and what a database driver's rows give; children share the tape,
`clone()` is O(1)), or a tree of nodes (`std/encoding/json/tree.velt`: what `JsonValue.object()`,
`set` and the other builders make).
- **Copy-on-write.** Values share nodes (a clone, a parent and the child `get` returned): an edit
  changes a node in place only while no other reference reaches it (the std-only intrinsic
  `isUnique`, as Rust's `Arc::get_mut`), and copies it first otherwise, the copy sharing the
  children.
- **A tree stays shallow.** A child may be a container left on a tape: an edit of a parsed value
  makes only its own node (its containers stay on the tape until they are edited), and a parsed
  value stored in a tree stays on its tape. A subtree as high as 512 nodes is stored as a tape
  before it goes in (`tree_freeze.velt`), made from its values, so numbers keep their bits (NaN,
  the infinities, -0). A tape is frozen and crosses to another task through its atomic count, so
  a tree sent there (a deep copy, where the sender keeps a reference) is copied at most 512 nodes
  deep: the copy recurses once per level.
- **Freezing costs O(n log n).** A child already on a tape is copied onto the new tape only while
  its tape is smaller than twice the new one; a larger one stays a node the new tape links to
  (`T_LINK`, resolved by the tree's readers). So a value is copied O(log n) times however deep a
  tree grows, and a chain of links stays O(log n) long.
- **Objects** (`tree_object.velt`) keep JavaScript's order (index keys first, apart and sorted,
  made only when the first one goes in), are found by key through a hash index past 16 members,
  and are deleted in O(1) amortized by leaving holes, with a Fenwick tree of the live slots for
  positional access while there are holes in the middle (`tree_positions.velt`).
- **Text.** A tree is written into a byte buffer handed to the builder in chunks, as a tape is.
  `console.log` prints either form through `JsonValue[JSON_INSPECT]` (`value_inspect.velt`).

### Numbers

- **Reading** (`std/lang/number/parse.velt`), correctly rounded for every input as V8 is: exact
  arithmetic when the mantissa is at most 2^53 and the power of ten at most 10^22 (Clinger),
  else Eisel-Lemire with its 128-bit power-of-five table (`eisel_lemire.velt`). More than 19
  significant digits, or a rounding Eisel-Lemire cannot decide, goes to an exact big-decimal
  comparison, also in Velt (`std/lang/hooks/number_parse.velt`), loaded only by programs that
  reach it. `Number()` and `parseFloat` use the same code.
- **Writing** (`std/encoding/json/writer_number.velt`) as `Number.prototype.toString` does
  (`1e+21`, `1e-7`, `0.000001`; `-0` as `0`; `NaN` and `±Infinity` as `null`): integers and
  short decimals directly, any other double through the runtime's shortest-digits printer.

### Strings

The scanner (`std/encoding/json/scan.velt`) looks for the end of a string 16 bytes at a time
with `u8x16` compares (`"`, `\`, bytes below 0x20), counting UTF-16 units as it goes: a block of
ASCII is 16 units, and only blocks with bytes of 0x80 or more adjust the count. The same Velt
source becomes NEON or SSE2 code. The writer (`writer.velt`) escapes as `JSON.stringify` does.

### Options, replacers and revivers

- **`JsonParseOptions`** (`std/prelude/json/json_error.velt`), for `JSON.parse<T>` and
  `JSON.parseValue`: `maxDepth`; `unknownKeys: "reject"`; `comments: "skip"`, which reads `//`
  and `/* */` as whitespace; `trailingCommas: true`, which accepts a comma before `]` and `}`.
  STJ calls the last two `ReadCommentHandling` and `AllowTrailingCommas`.
- **Replacer functions.** `JSON.stringify(x, replacer, space)` takes an array of keys, `null`,
  or a function whose value parameter is annotated `JsonValue`:
  `(key: string, value: JsonValue) => JsonValue | undefined` (the prelude type `JsonReplacer`).
  The value is written by its typed code first, so `toJSON` has already run, as in Node; the
  replacer is then called as Node calls it (the value itself under the key `""` first, then the
  members of each result; `undefined` drops a member and writes an array element as `null`). It
  gets no `this`.
- **Revivers.** `JSON.parseValue(text).revive(reviver)` takes the same function type, in Node's
  order (innermost first, array keys as strings, `undefined` deletes a member, a deleted array
  element is `null`).
- **The value parameter must be annotated.** A callback written as untyped TypeScript
  (`(k, v) => typeof v === "number" ? v * 2 : v`) must not compile to something that silently
  does less than in Node, so an unannotated value parameter is a compile error until `any` has
  its JavaScript meaning ([#924](https://github.com/velt-lang/velt/issues/924)).
- `undefined` for the root, where Node returns `undefined`, throws a `TypeError` that says so,
  since Velt's results are a `string` and a `JsonValue`.

## Compilation

| Part | Where |
|---|---|
| `JSON`, `JsonValue`, `JsonError`, options, replacer layout | `std/prelude/json/` (loaded only when a module names one of them: `std/prelude/global/json.velt`) |
| Per-type decoders and writers | generators in `std/encoding/json/generate/` (`read*.velt`, `write.velt`, `check.velt`), declared in `std/encoding/json/generated.velt` |
| Scanner, pull reader, writer | `std/encoding/json/scan*.velt`, `reader*.velt`, `writer*.velt` |
| Reader of a native driver's rows (`decodeRows<T>` of a row type not read by column) | `std/encoding/json/rows_reader.velt`, `rows_cells.velt` |
| Tape | `std/encoding/json/tape*.velt` |
| Node's syntax-error messages (computed only after a failure) | `std/encoding/json/node_syntax.velt` |
| `JSON.stringify(value, keys, space)` | `std/encoding/json/relayout.velt` |
| Tree of a built or edited `JsonValue`, and what `console.log` prints for one | `std/encoding/json/tree*.velt`, `value_inspect.velt` |
| Numbers | `std/lang/number/`, `std/lang/hooks/number_parse.velt` |

No JSON code is left in the runtime (#3318, #3530): no parsing, reading, writing or escaping.
What std calls there is the string builder's (appending bytes and strings) and velt/db's rows
functions (`velt_rt_rows_*`, [rt_abi_async.md §12.1](../contracts/rt_abi_async.md)), through
which `rows_reader.velt` reads a driver's cursor: a JSON cell's text and a text cell's string,
which std escapes.

## Compatibility

What the engine keeps of Node's behavior, and what checks it. `bench/json/fuzz/run.sh` runs each
fuzz program under Node and under Velt (debug with the checking allocator, and release) and
requires identical bytes.

| Behavior (Node's) | Checked by |
|---|---|
| Strings escaped as `JSON.stringify` does: `"`, `\`, controls, lone surrogates as `\udxxx`; U+2028/2029 raw | fuzz `strings` |
| Numbers written as `Number.prototype.toString` does; `-0` as `0`; `NaN`, `±Infinity` as `null` | fuzz `numbers` |
| Numbers read correctly rounded (`12345678901234567890123`, `1e400`, `-0`, `1e-400`, subnormals) | fuzz `to_number`; golden `std/json_float_parse` |
| Key order: index-like keys first, ascending; a repeated key keeps its last value in its first place; `__proto__` is an ordinary key | fuzz `values`; goldens `json_duplicate_keys`, `record_json_index_order` |
| Syntax errors: Node's message and UTF-16 position | goldens `json_parse_syntax_node`, `json_error_paths` |
| `toJSON(key)` runs before a replacer; `undefined`, functions and symbols skipped in objects, `null` in arrays | goldens `json_to_json`, `any_json_undefined`, `void_field_any_json` |
| Getters of object literals run and are written; class prototype getters are not; symbol keys are skipped | goldens `any_literal_accessor_json_only`, `any_literal_getter_unions`, `symbol_keys` |
| Proxies go through `ownKeys`, `getOwnPropertyDescriptor`, `get` | golden `proxy_json` |

Differences from Node:

- Writing a cycle stops the program: an `any` value with `TypeError: Converting circular
  structure to JSON`, a typed recursive value with `JSON.stringify: converting circular structure
  to JSON (an object contains itself)`. Neither has Node's `--> starting at …` lines.
- A type with no JSON form (`bigint`, functions, a std type's private state) is a compile error
  at the call, not a run-time `TypeError`.
- `JSON.parse(text, reviver)` does not exist; `JSON.parseValue(text).revive(reviver)` does what
  it does. Node's third reviver argument (`context`) is not given.
- `u64` fields read and write their whole range.

## Runtime cost

A program that does not name `JSON`, `JsonValue` or `JsonError` loads none of the engine. A
program that does compiles one decoder or writer per type it parses or writes, and the reader
and writer are compiled into it, so debug builds (`velt run`, the JIT) run an unoptimized engine.
Changes to the engine are measured by instructions retired, base against head, on
`bench/json/run.sh` (`parse_typed`, `parse_union`, `stringify`, `parse_value`, `navigate`,
`edit`, `build_write`) and on the corpora of `bench/json/corpora/` (`twitter.json`, `citm_catalog.json`,
`canada.json`, fetched by `fetch.sh` with pinned SHA-256s; dynamic parse and write against
serde_json, sonic-rs, simd-json and Node), together with build time and IR size. Results are in
`bench/RESULTS.md`.

## Alternatives considered

**The engine in Rust in the runtime.** Every token would cross an extern call LLVM cannot
inline, and the speed it showed would be Rust's, not Velt's.

**STJ contract customization** (`IJsonTypeInfoResolver` modifiers that change contracts at run
time). Velt contracts are fixed when the program is compiled, which is where the speed comes
from. Converters cover the cases modifiers are used for.

**STJ `ReferenceHandler.Preserve`** (`$id`/`$ref`). A graph format is not JSON's job; Velt
refuses cycles.

**A program-wide naming policy in the manifest.** STJ has `JsonSerializerOptions` defaults, but
a manifest setting would let a dependency's JSON shape depend on the program that imports it.
Naming policies are per class.

## Limitations

- A `JsonValue` from `parseValue` keeps its whole source text alive while any value of the
  document is alive: a small value can pin a large document.
- `Request.json<T>()` and `Response.json<T>()` make a `string` of the body before parsing, and
  `Response.json(data)` makes the whole text before sending it.
- Decoded arrays grow one `push` at a time, with no size hint.
- The scanner skips whitespace one byte at a time, and digits are read one at a time.
- A `Record` with literal keys tracks required keys only up to 64 of them.

## Not yet implemented

### Faster key matching

The decoder checks the field that follows the last one decoded first, since keys usually arrive
in declaration order, then switches on the key's length and compares 2-, 4- or 8-byte words
loaded from the key's bytes, emitted inline:

```text
key = next_key()                    // borrowed: points into the source
if key == field[expect] { decode field[expect]; expect += 1; continue }   // usually taken
switch key.len {
  2: if load_u16(key) == "id"   { … }
  4: if load_u32(key) == "name" { … }
  9: if load_u64(key) == "createdA" && key[8] == 't' { … }
}
unknown key
```

A key that arrives in declaration order costs one length compare and one or two word compares.
The code per decoder grows by about one word compare per field; there is no run-time cost.

### Scanner and arrays

- **Whitespace:** a 16-byte skip for indented JSON after the one-byte fast exit.
- **Digits:** eight at a time (SWAR) before the general path, as fast_float and STJ's
  `Utf8Parser` do.
- **Array capacity:** a decoded `T[]` starts at the capacity the last array decoded at the same
  depth by the same decoder ended with, a hint kept in the reader's context, avoiding most
  regrowth on documents of many similar arrays without scanning ahead.
- **Writing doubles** with std's shortest-digits code (`std/lang/number/shortest.velt`) instead
  of the runtime's.

### Parsing bytes

`JSON.parse<T>(bytes, options?)` and `JSON.parseValue(bytes)` take a `Uint8Array`, an
`ArrayBuffer` or a `u8[]` and parse it as UTF-8 directly. The scanner checks UTF-8 inside its
string scan (non-ASCII blocks only); invalid UTF-8 is a `JsonError` giving the byte offset.
`Request.json<T>()` and `Response.json<T>()` parse the body bytes without making a `string`:

```ts ignore
const cfg = JSON.parse<Config>(await readFileBytes("config.json")); // bytes in, no string made
```

The overload is chosen by the static type: a `JSON.parse(x)` where `x: any` holds a `Uint8Array`
converts it to a string first, as Node does. Differences from Node's `JSON.parse(buf.toString())`:
invalid UTF-8 is a `JsonError` (Node decodes it as U+FFFD); a byte order mark is an error, as in
Node's `JSON.parse`; error positions are byte offsets.

### Writing to a sink

`JSON.stringifyTo(out, value)` writes into a byte sink (a `WritableStream<Uint8Array>`, a socket,
a file or a `velt/io` writer) without making a `string`; the buffer drains whenever it passes
64 KiB, as STJ's `Utf8JsonWriter` does over a `PipeWriter`. `Response.json(data)` stays eager: in
Node it throws for a cycle at the call, before the headers are sent, and runs `toJSON` and
getters then, with a `Content-Length`. Streaming is for code that asks for it.

### Streaming

```ts ignore
for await (const order of JSON.parseStream<Order>(res.body!)) { … }        // top-level array
for await (const line of JSON.parseLines<LogLine>(file.readable())) { … }   // NDJSON
```

The reader keeps one partly read element and a small buffer, never the document. The pull reader
gains a "need more input" state, so a decoder stops and resumes between two elements without
coroutines in the generated code: each element is decoded only once its bytes are complete.
`parseStream<T>` is STJ's `DeserializeAsyncEnumerable<T>`.

### JSON decorators

Velt has TypeScript's decorator syntax for compile-time decorators, declared with
`declare decorator function` ([#2203](https://github.com/velt-lang/velt/issues/2203)): a decorator
is data with constant arguments, checked by sema, and nothing runs at run time. The JSON
decorators are such declarations in `velt/encoding/json`'s `json` namespace. They need the
source-generator model to carry decorator uses (see
[source-generators.md](source-generators.md#not-yet-implemented)), so that the JSON generators
read them and report misuse at the decorator.

```ts ignore
import { json } from "velt/encoding/json";

@json.naming("snake_case")
class User {
  id: i64 = 0;                         // "id"
  @json.name("display_name") name: string = "";
  createdAt: string = "";              // "created_at"
  @json.ignore({ when: "null" }) email: string | null = null;
  @json.extra rest: Map<string, Value> = new Map();
}
```

| Decorator | On | Effect | STJ |
|---|---|---|---|
| `@json.name("k")` | field | Name in JSON | `JsonPropertyName` |
| `@json.naming("camelCase" \| "snake_case" \| "kebab-case" \| "PascalCase" \| "SCREAMING_SNAKE_CASE")` | class | Names of all fields, at compile time | `PropertyNamingPolicy` |
| `@json.ignore()` / `@json.ignore({ when: "null" \| "default" })` | field | Never written or read / not written when null or the type's default | `JsonIgnore` |
| `@json.required` | field | Absent key is an error even if the type has a default | `JsonRequired` |
| `@json.unknownKeys("reject" \| "skip")` | class | Per-type `unknownKeys` | `UnmappedMemberHandling` |
| `@json.extra` | field of `Map<string, Value>` | Unknown members go here, and are written back as members of the object (its entries, as `Object.fromEntries` or a `Record` writes them: `JSON.stringify` of the `Map` itself is `{}`, as in Node) | `JsonExtensionData` |
| `@json.numbers("string" \| "allowString")` | field or class | Write `i64`/`u64` as strings; accept numbers in strings | `JsonNumberHandling` |
| `@json.discriminator("kind", { A: "a", B: "b" })` | base class | Polymorphic subclasses by a tag, written first | `JsonPolymorphic`, `JsonDerivedType` |
| `@json.converter(C)` | field or class | `C` has static `read(r: json.Reader): T` and `write(w: json.Writer, v: T): void`; the generated code calls them directly (inlinable, not virtual) | `JsonConverter` |
| `@json.populate` | field | Decode into the existing value (keep defaults, append to collections) | `JsonObjectCreationHandling.Populate` |

Rules:

- Decorators go on classes, structs and their fields; TypeScript allows none on interfaces or
  type aliases, so an object type that needs options becomes a class or struct.
- A subclass inherits field decorators. A class decorator applies to the class's own fields.
- Misuse is a compile error naming the decorator and the fix: `@json.extra` on a field that is
  not a `Map<string, Value>`; two fields with the same JSON name after `@json.naming`; a
  discriminator value used twice; `@json.converter(C)` where `C` lacks `read` or `write`.
- A converter is how a field of a type with no JSON form (`bigint`, `Date` when decoding)
  decodes.

### Schema and the low-level API

- **`JSON.schema<T>(): string`** returns the JSON Schema (draft 2020-12) of what
  `JSON.parse<T>` accepts: required keys, literal unions (`enum`), discriminators (`oneOf` with
  `const`), decorators and `unknownKeys`. It is generated from the same model as the decoder, so
  the two cannot drift apart; the result is a string constant.
- **`json.Reader` and `json.Writer`** become public in `velt/encoding/json`: the pull reader
  (`std/encoding/json/reader.velt`'s `Reader`) and the writer the generated code uses, and the
  types converters receive. With them, hand-written parsers run at the speed of generated ones,
  as STJ's `Utf8JsonReader` allows.

### Other

- **Cycle paths:** Node's `--> starting at …` / `--- property … closes the circle` lines in the
  cycle error ([#1701](https://github.com/velt-lang/velt/issues/1701)).
- **`JSON.parse(text, reviver)`** and untyped replacer and reviver callbacks with Node's meaning,
  once `any` has its JavaScript meaning ([#924](https://github.com/velt-lang/velt/issues/924));
  the `JsonValue` forms stay.
- **Typed-parse rows and a Bun column** in the corpora benchmark.

Open questions for these parts:

1. Whether interfaces and type aliases also get options, through a value such as
   `JSON.codec<T>({ naming: "snake_case", … })`, or "use a class or struct" is enough.
2. Whether `JSON.parse<T>` gets an option that rejects duplicate keys, as STJ's
   `AllowDuplicateProperties = false`.
3. Whether a `JsonValue` gets an explicit `detach()` that copies what it holds out of a large
   source, as STJ's `JsonElement.Clone()`.
