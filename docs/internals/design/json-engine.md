# Design: the JSON engine, fastest in class with System.Text.Json's features

Status: proposed. Nothing here is implemented beyond what [velt/encoding/json](../../std/json.md)
documents today. Tracking: #1655, with one sub-issue per step; survey #529 has the `navigate` and `parse_value` rows.

## Where we are

Velt's JSON is already built the way .NET's System.Text.Json (STJ) is built when it uses its
source generator: there is no reflection at run time. `JSON.parse<T>` and `JSON.stringify<T>`
compile to one decoder or writer per type (`crates/velt_vir/src/lower/json/`), on top of a pull
reader and a string builder in the runtime (`crates/velt_rt/src/json/reader.rs`,
`crates/velt_rt/src/strbuf.rs`).

`bench/json/run.sh` on an Apple M4 (`bench/RESULTS.md`, latest table):

| benchmark | Velt / serde_json | Velt / Node |
|---|---:|---:|
| parse_typed | 0.62 | 0.49 |
| parse_union | 1.21 | 0.64 |
| stringify | 1.50 | 0.83 |
| parse_value | 1.76 | 1.27 |
| navigate | 0.64 | 1.63 |

Typed parsing is ahead of both. The weak spots are listed below, each with where it comes from.

- **Matching a key to a field.** Each key is compared with every field name in turn, one
  `velt_rt_str_eq` call per field (`object.rs`, `json_read_member`). A 12-field object costs
  up to 12 calls for each key.
- **The dynamic tree.** `JSON.parseValue` builds a tree of `Arc<Value>` nodes of 40 bytes each,
  with a boxed copy of every key, and a `HashMap` index once an object has more than 16 keys
  (`velt_rt/src/json/value.rs`, `object.rs`). Every node is a separate allocation, and every
  `get` clones an `Arc`.
- **The scanner.** It reads one byte at a time with a lookup table (`velt_text/src/json_scan.rs`).
  The only SIMD on the JSON path is in the stringify escape scan (`json_escape.rs`, `clean_run`).
- **Numbers.** Integers of 15 digits or fewer take a fast path; every other number goes through
  `str::parse::<f64>`.
- **Arrays.** Decoded arrays grow one `push` at a time, with no size hint.
- **Text only.** `req.json<T>()` and `res.json<T>()` first make a `string` of the body: a copy,
  a UTF-8 check and a UTF-16 count. Then they parse. `Response.json` makes the whole string
  before sending it, and nothing can parse a stream.
- **Options.** There is no rename, naming policy, conditional ignore, required field,
  polymorphic discriminator, custom converter or extension data. There is no reviver and no
  replacer function.

The goal: the fastest JSON in any language for typed data, faster than Node for dynamic data,
and the features of STJ.

## What we borrow, and from where

| From | Idea | In Velt |
|---|---|---|
| STJ source generator (`JsonSerializerContext`) | Contracts fixed at compile time, a fast write path that writes literal key text | Already Velt's model. Options (below) feed the same glue |
| STJ property cache | Expect the next key to be the next field in declaration order, check that first, then fall back to a lookup | Step 1 |
| serde, sonic-rs | Match keys on length, then compare the bytes in 8-byte words, not one call per field | Step 1 |
| simdjson, sonic-rs, STJ's vectorized `IndexOfAny` | Find string ends and skip whitespace 16 or 32 bytes at a time | Step 3 |
| fast_float, STJ `Utf8Parser` | Parse 8 digits at once (SWAR) before the general path | Step 3 |
| STJ `JsonDocument` (one flat array of rows pointing into the pooled UTF-8 source), simdjson's tape | A flat tape for the dynamic value: no allocation per node; a string is made only when read | Step 5 |
| STJ `Utf8JsonReader`/`Utf8JsonWriter` | Parse UTF-8 bytes as they arrive; write into a buffer that drains to a socket | Steps 6, 7, 12 |
| STJ `DeserializeAsyncEnumerable` over a `PipeReader` | Decode the elements of a large top-level array as they stream in | Step 8 |
| STJ attributes | `[JsonPropertyName]`, `[JsonIgnore(Condition = …)]`, `[JsonRequired]`, `[JsonExtensionData]`, `[JsonNumberHandling]`, `[JsonConverter]`, `[JsonPolymorphic]` + `[JsonDerivedType]`, `[JsonObjectCreationHandling]` | `@json.*` decorators, steps 9–10 |
| STJ options | `PropertyNamingPolicy`, `DefaultIgnoreCondition`, `UnmappedMemberHandling`, `ReadCommentHandling`, `AllowTrailingCommas` | Class decorators (compile time) and `JSON.parse` options (step 11) |
| STJ `JsonSchemaExporter` | A JSON Schema made from a type | `JSON.schema<T>()`, a constant built at compile time (step 12) |

**Not borrowed:**
- **STJ contract customization** (`IJsonTypeInfoResolver` modifiers that change contracts at run
  time). Velt contracts are fixed when the program is compiled, which is where the speed comes
  from. Converters cover the cases modifiers are used for.
- **STJ `ReferenceHandler.Preserve`** (`$id`/`$ref`). Velt keeps refusing cycles, with
  `TypeError: Converting circular structure…`. A graph format is not JSON's job.

## The design

### Rule: the engine is written in Velt

Every part of the engine is Velt code in `std/`, built only on APIs every program has (`u8[]`
and byte views, `velt/simd`, integer types, classes and structs). This includes the scanner,
the pull reader, number parsing and printing, string escaping, `JsonValue` and the writer. It
is the rule `BTreeMap` follows (#1543). No logic goes into new Rust code in `velt_rt` or
`velt_text`, and no Rust crate is used. The reasons:

- **It proves the claim.** If `velt/encoding/json` is Velt and beats serde_json, the language is fast.
  If it were Rust behind an extern call, that would only show that Rust is fast.
- **The code is faster.** The per-type glue calls today's runtime through opaque extern calls
  (`velt_rt_json_reader_next_key`, `_read_f64`, `velt_rt_str_eq` per field). LLVM can inline
  none of them. A reader written in Velt is compiled with the glue, so it is inlined,
  specialized per type and register-allocated as one unit.
- **Gaps are fixed in the compiler.** Where Velt is slower than the Rust it replaces (bounds
  checks, SIMD lowering, inlining, layout), the fix goes into the compiler or the language and
  speeds up every program. A Rust fast path would hide the gap.
- **One language and one ABI.** The same code runs on every target, wasm included, with no
  runtime ABI to keep in step.

The glue that VIR lowering generates for each type stays compiler-generated. It calls the
Velt reader's methods, which it finds the way lowering already finds prelude items such as
`Proxy.__view`, instead of `Rt::Json*` externs.

### Compatibility contract, and how it is checked

The engine is rewritten, so what it must keep is listed here. Each item is checked by a golden
or by a Node-differential fuzz in `bench/json/fuzz/` (`run.sh` runs each program under Node and
under Velt, debug with the checking allocator and release, and compares the bytes):

| Behaviour (Node's) | Checked by |
|---|---|
| Strings escaped as `JSON.stringify` does: `"`, `\`, controls, lone surrogates as `\udxxx`; U+2028/2029 raw | fuzz `strings` (125,000 cases; from #1066) |
| Numbers written as `Number.prototype.toString` does; `-0` as `0`; `NaN`, `±Infinity` as `null` | fuzz `numbers` (400,000 doubles, every binary exponent) |
| Numbers read correctly rounded (`12345678901234567890123`, `1e400`, `-0`, `1e-400`, subnormals) | fuzz `to_number` (60,000 strings through `Number`, `parseFloat`, `JSON.parse`); golden `std/json_float_parse` (with the stack's float parsing) |
| Key order: index-like keys first, ascending; a repeated key keeps its last value in its first place | fuzz `values` (20,000 documents); goldens `json_duplicate_keys`, `record_json_index_order` |
| `__proto__` is an ordinary own key when parsing | to add to fuzz `values` |
| Syntax errors: Node 26's message and UTF-16 position | goldens `json_parse_syntax_node`, `json_error_paths` |
| `toJSON(key)` runs before a replacer function; `undefined`, functions and symbols skipped in objects, `null` in arrays | goldens `json_to_json`, `any_json_undefined`, `void_field_any_json` (the replacer's order: with step 11's golden) |
| An invalid `Date` writes `null` | to add (no golden covers it today) |
| Getters of object literals run and are written; class prototype getters are not; symbol keys are skipped | goldens `any_literal_accessor_json_only`, `any_literal_getter_unions`; `symbol_keys` |
| Proxies go through `ownKeys`, `getOwnPropertyDescriptor`, `get` | golden `proxy_json` |
| An `any` holding a `Map` or `Set` writes `{}` (a typed `Map` writes its entries, a documented difference) | to add (no golden covers it today) |

Not Node's yet:
- A cycle throws `TypeError: Converting circular structure to JSON` without Node's
  `--> starting at …` / `--- property … closes the circle` lines. Step 0 owns the writer's path
  tracking, so it gets them (#1701).
- A `BigInt` has no JSON form: a compile error, not Node's run-time `TypeError`. The fuzz runner does not run wasm32 yet (#1066's did): it gets a wasm32
mode when wasmtime is on the machine.

### 0. Move the reader, scanner and writer into Velt (no API change)

Port `velt_text/src/json_scan.rs`, `velt_rt/src/json/reader.rs`, `json_escape.rs`, the JSON
pushes of `strbuf.rs` and float printing (`ryu`, as a std port) to Velt, as internal std modules
in `std/json/` (`velt/encoding/json/scan`, `velt/encoding/json/reader`, `velt/encoding/json/writer`, `velt/encoding/json/tape`),
which the prelude's JSON module imports (`std/prelude/json/json.vlt`, a global a program loads
only when its code names `JSON` or `JsonValue`: std/prelude/global/json.vlt). Then switch the
glue in `crates/velt_vir/src/lower/json/` from `Rt::Json*` calls to calls of those Velt methods.

Error messages and paths stay the same, and the golden tests do not change. Node's exact
syntax errors (`node_syntax.rs`) can stay a Rust slow path for now, as they run only after a
failure. The database row readers and native packages keep their own Rust copy until each is
ported. The Rust JSON code is deleted once nothing calls it.

**Gate** (every step, against `main` at the step's merge base, so allowances cannot add up):
- instructions retired, base against head on the same machine (cachegrind on Linux,
  `/usr/bin/time -l` on macOS), at most 3% worse per row; `bench/json/run.sh` wall times are
  the second view (`parse_value` varies 0.9-2.7 s from run to run on a loaded machine, so wall
  clock cannot resolve 3%);
- the JSON benchmarks, and `bench/compare_refs.sh` for the other programs on the path:
  `json_to_json`, the web server, `bench/typical`;
- debug builds too: the Velt engine is compiled into each program, so `velt run`, the goldens
  and the JIT get an unoptimized engine. The gate adds JSON speed in a debug build, compile time
  (`compile_speed`) and binary size; if debug speed regresses badly, std's JSON modules get
  optimized in debug builds;
- the Node-differential fuzz (`bench/json/fuzz/run.sh`) passes, and the escape kinds benchmark
  (`bench/json/fuzz/escape_kinds.vlt`, #1066's `kinds.vlt`) is no slower per shape;
- every gap found is filed and fixed in the compiler, not with a Rust fast path.

### 1. Matching a key to a field (no API change)

An object decoder today does this for each key:

```text
key = next_key()
if str_eq(key, "id")    { … }
if str_eq(key, "name")  { … }
…                                   // N calls in the worst case
```

It becomes:

```text
key = next_key()                    // borrowed: points into the source
if key == field[expect] { decode field[expect]; expect += 1; continue }   // usually taken
switch key.len {
  2: if load_u16(key) == "id"   { … }
  4: if load_u32(key) == "name" { … }
  9: if load_u64(key) == "createdA" && key[8] == 't' { … }
  …
}
unknown key
```

Both the "expected" compare and the switch are emitted as inline VIR over the borrowed key's
bytes, not runtime calls. A key
that arrives in declaration order costs one length compare and one or two word compares. The
`seen` bitmask becomes several words past 64 fields; today a class with more than 64 fields is
an ICE (`object.rs`).

A key with an escape (`"n\u0061me"`) is decoded first and compared decoded, so it matches
`name` as in Node. The expected-field fast path still sets and checks the `seen` mask: a
repeated key keeps the current rule (its last value, in the first one's place, every
occurrence validated).

**Cost.** The code per decoder grows by about one word compare per field, and there is no
runtime cost. The PR reports the change in build time and IR size.

### 2. Benchmarks that others publish

Add the corpora every JSON library reports on to `bench/json/corpora`:
- `twitter.json`: mixed, with non-ASCII strings;
- `citm_catalog.json`: wide objects and many integers;
- `canada.json`: floats.

`bench/json/corpora/fetch.sh` downloads them with pinned SHA-256s; they are not committed.
`bench/json/corpora/run.sh` next to it runs each corpus as a typed parse, a `parseValue`, and a
stringify of the result. The comparison rows are serde_json, sonic-rs and simd-json (Rust), Node
and Bun.

Landed (#1746): the `parseValue` and stringify rows against serde_json, sonic-rs, simd-json and
Node, with the first results in `bench/RESULTS.md` ("JSON corpora"). Still to come: the typed
parse rows and the Bun column.

Targets:
- typed parse faster than serde_json on all three corpora, and at sonic-rs's speed on
  `citm_catalog`;
- `parseValue` and `navigate` faster than Node;
- stringify faster than serde_json.

### 3. A SIMD scanner and SWAR numbers

The scanner from step 0 is written with `velt/simd` (`u8x16`, compare and movemask), so the
same Velt source becomes NEON or SSE2 code. A missing vector operation is added to `velt/simd`
and the backends, not written in Rust:
- **The inside of a string.** It looks 16 bytes at a time (16-byte vectors) for `"`, `\`,
  bytes below 0x20 and bytes of 0x80 or more. While doing so it counts UTF-16 units, so the
  table lookup runs only on blocks that are not ASCII. This follows the shape of
  `json_escape.rs`'s `clean_run`.
- **Whitespace.** A fast exit when the next byte is not whitespace (the common case in compact
  JSON), and a 16-byte skip for indented JSON.
- **Reading numbers**, correctly rounded for every input as V8 is, in Velt (`velt/lang/number`):
  exact arithmetic when the mantissa is at most 2^53 and the power of ten at most 10^22
  (Clinger), else Eisel-Lemire with its 128-bit power-of-five table. More than 19 significant
  digits, or a rounding Eisel-Lemire cannot decide, still goes to the runtime's parser: a
  temporary exception to the rule above, until a Velt big-decimal slow path (fast_float's
  "digit comparison", #1702) replaces it. Eisel-Lemire's 64×64→128-bit products use `umulh`
  (the prelude's `Math.umulh`, one instruction in both backends), so it needs no compiler
  change. `Number()` and `parseFloat` use the same code.
- **Integers.** A `u64` field reads its full 20-digit range (today it stops at `i64::MAX`, a
  limit this design removes).
- **Writing numbers** as `Number.prototype.toString` does (`1e+21`, `1e-7`, `0.000001`, `-0`
  as `0`): Ryu's shortest digits, in Velt, laid out by JavaScript's rules. An `f32` field is
  written as the `f64` it widens to (`0.10000000149011612` for `0.1`), as JavaScript writes
  `Math.fround(0.1)`.


### 4. Arrays

A decoded `T[]` grows by doubling, as today, but starts at the capacity the last array decoded
at the same depth by the same decoder ended with. That is a hint kept in the reader's context
and costs nothing to keep. It avoids most regrowth on documents of many similar arrays without
scanning ahead.

### 5. A tape for `JsonValue`

`JSON.parseValue` writes a **tape**, a Velt class in `velt/encoding/json/tape`: two `u64` words per
value in one array. The first word is the kind, a flag (escaped string, repeated keys), and
from bit 32 a string's UTF-16 length or a container's member count; the second is a number's
bits, a string's byte range in the source (`start | end << 32`) or its index among the decoded
strings, or a container's first position in a second array that lists each container's members
together (so `at(i)` is O(1)). Limits: 2^32 bytes of source and 2^32 members per container,
both past V8's string limit (and Velt's 2 GiB one); there is no smaller bound to document.

**JavaScript order.** The tape keeps source order. An object whose keys are all in JavaScript
order already (no index-like key, no repeated key, checked in place while parsing) uses it as
it is. Any other object gets its member list in JavaScript order when it closes (index keys
first, ascending, then the rest; a repeated key at its first place, with its last value), which
`keys()`, `at(i)`, `len()` and `stringify()` read.

**Representation.** A `JsonValue` is a class with two forms: a position on a tape (what parsing
makes; children share the tape, `clone()` is O(1)), or a runtime tree (what `object()`,
`set`, … make, and a driver's rows). An edit turns that one value into a tree first, so it never
shows through the tape, a clone or a child: the value semantics stay as today.

The Rust `JsonValue` (`velt_rt/src/json/value*.rs`) and its ABI in
`docs/internals/contracts/rt_abi_async.md` §12 go away once nothing calls them (edits and rows
still do). This is STJ's split between `JsonDocument`/`JsonElement` (read-only, flat) and
`JsonNode` (mutable), with Velt keeping one type in front of both.

### 6. Parsing bytes

The new overloads parse a `Uint8Array`, an `ArrayBuffer` or a `u8[]` as UTF-8 directly:
- `JSON.parse<T>(bytes, options?)`
- `JSON.parseValue(bytes)`

The scanner already works on bytes. It adds a UTF-8 check that runs inside the string scan
(non-ASCII blocks only), and invalid UTF-8 is a `JsonError` that gives the byte offset. Changes
in std:
- `Request.json<T>()` and `Response.json<T>()` parse the body bytes, without making a `string`;
- `velt/fs`'s `readFileBytes` paired with `JSON.parse<T>` documents the pattern.

```ts ignore
const cfg = JSON.parse<Config>(await readFileBytes("config.json")); // bytes in, no string made
```

The overload is chosen by the static type only: a `JSON.parse(x)` where `x: any` holds a
`Uint8Array` converts it to a string first, as Node does. Differences from Node's
`JSON.parse(buf.toString())`, documented: invalid UTF-8 is a `JsonError` (Node decodes it as
U+FFFD); a byte order mark is an error, as in Node's `JSON.parse`; error positions are byte
offsets (there is no string to count UTF-16 units in).

### 7. Writing to a sink

`JSON.stringifyTo(out, value)` writes into a byte sink without making a `string`. The sink is a
`WritableStream<Uint8Array>`, a socket, a file, or a `velt/io` writer. The writer glue already
appends to a `strbuf`; the sink's buffer drains whenever it passes 64 KiB, as STJ's
`Utf8JsonWriter` does over a `PipeWriter`. `Response.json(data)` stays eager: in Node it throws
for a cycle or a `BigInt` at the call, before the headers are sent, and runs `toJSON` and
getters then, with a `Content-Length`. Streaming is for code that asks for it.

### 8. Streaming

```ts ignore
for await (const order of JSON.parseStream<Order>(res.body!)) { … }        // top-level array
for await (const line of JSON.parseLines<LogLine>(file.readable())) { … }   // NDJSON
```

The reader keeps one partly read element and a small buffer, never the document.
`parseStream<T>` is STJ's `DeserializeAsyncEnumerable<T>`. The pull reader gains a "need more
input" state, so that a decoder can stop and resume between two elements without coroutines in
the generated code: each element is decoded only once its bytes are complete.

### 9. Decorators (compile time only)

Velt has no decorators today (`docs/book/ts-developers.md`). This step adds the TypeScript
syntax for them, with the meaning of .NET attributes:

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

The rules for v1:

- **The syntax is TypeScript's** (TC39 stage 3). `@expr` goes before a class, a field, a
  method or an accessor. A `.vlt` file still parses as TypeScript.
- **A decorator is data, not a call.** It must name a function that std marks as a compile-time
  decorator (a `@decorator` doc tag on a declaration in `std/`, read by sema). Its arguments
  must be constants: literals, object and array literals of literals, enum members or type
  names.
- **Sema records it** as metadata on the HIR item. The VIR JSON lowering reads that metadata
  when it builds a decorated type's glue.
- **Nothing runs at run time.** A program that does not use JSON pays nothing.
- **A user's own decorator is a compile error in v1:** `decorators that run code are not
  supported yet; std's compile-time decorators are`. Runtime TC39 decorators can come later on
  the same syntax.
- **Decorators are allowed on** classes, structs, and fields of either. They are not allowed on
  interfaces or type aliases, which TypeScript does not allow either. An object type that needs
  options becomes a class or struct.
- **Inheritance:** a subclass inherits field decorators. A class decorator applies to the
  class's own fields only, as STJ's `[JsonSerializable]` contexts apply per type.

**Contracts.** This changes `ast.rs` (decorators on class members and classes) and the HIR
item types, both fixed contracts. It needs a maintainer's sign-off before the step starts, and
`docs/internals/contracts/` documents the new fields.

### 10. The JSON decorators

The decorators live in `velt/encoding/json`'s `json` namespace and are checked by sema.

| Decorator | On | Effect | STJ |
|---|---|---|---|
| `@json.name("k")` | field | Name in JSON | `JsonPropertyName` |
| `@json.naming("camelCase" \| "snake_case" \| "kebab-case" \| "PascalCase" \| "SCREAMING_SNAKE_CASE")` | class | Names of all fields, at compile time | `PropertyNamingPolicy` |
| `@json.ignore()` / `@json.ignore({ when: "null" \| "default" })` | field | Never written or read / not written when null or the type's default | `JsonIgnore` |
| `@json.required` | field | Absent key is an error even if the type has a default | `JsonRequired` |
| `@json.unknownKeys("reject" \| "skip")` | class | Per-type `unknownKeys` | `UnmappedMemberHandling` |
| `@json.extra` | field of `Map<string, Value>` | Unknown members go here, and are written back | `JsonExtensionData` |
| `@json.numbers("string" \| "allowString")` | field or class | Write `i64`/`u64` as strings; accept numbers in strings | `JsonNumberHandling` |
| `@json.discriminator("kind", { A: "a", B: "b" })` | base class | Polymorphic subclasses by a tag; written first | `JsonPolymorphic`, `JsonDerivedType` |
| `@json.converter(C)` | field or class | `C` has static `read(r: json.Reader): T` and `write(w: json.Writer, v: T): void`; the glue calls them directly (inlinable, not virtual) | `JsonConverter` |
| `@json.populate` | field | Decode into the existing value (keep defaults, append to collections) | `JsonObjectCreationHandling.Populate` |

Errors are compile errors that give the decorator and the fix. Some examples:
- `@json.extra` on a field that is not a `Map<string, Value>`;
- two fields with the same JSON name after `@json.naming`;
- a discriminator value used twice;
- `@json.converter(C)` where `C` lacks `read` or `write`.

Today a `bigint` or a `Date` has no JSON form when decoding. A converter is how such a field
decodes.

### 11. JSONC, reviver and replacer functions

- **`JSON.parse` options:**
  - `comments: "skip"` accepts `//` and `/* */`;
  - `trailingCommas: true` accepts a comma before `]` and `}`.

  STJ calls these `ReadCommentHandling` and `AllowTrailingCommas`. Both are flags in the reader,
  and cost one predictable branch when off.
- **Replacer functions and revivers**, until `JSON.parse` into `any` exists (#924). A callback
  written as TypeScript (`(k, v) => typeof v === "number" ? v * 2 : v`) must not compile to
  something that silently does less than in Node, so only the explicit `JsonValue` form is
  accepted for now:
  - `JSON.stringify(x, replacer, space)` takes a function whose value parameter is annotated
    `JsonValue`: `(key: string, value: JsonValue) => JsonValue | undefined` (the prelude type
    `JsonReplacer`). An unannotated `(k, v) => …` is a compile error that names #924 and says
    to annotate `v: JsonValue`. The value is written by its typed glue first, so `toJSON` has
    already run, as in Node; the replacer is then called as Node calls it (the value itself
    under the key `""` first, then the members of each result; `undefined` drops a member and
    writes an array element as `null`). It gets no `this` (the holder). A `BigInt`, function or
    `Map` cannot reach it: they have no JSON form in Velt.
  - `JSON.parseValue(text).revive(reviver)` takes the same function type, in Node's order
    (innermost first, array keys as strings, `undefined` deletes a member, a deleted array
    element is `null`). Node 26's third `context` argument (`source`) is not given yet.
    `JSON.parse(text, reviver)` itself waits for #924, since its result is `any` in TypeScript.
  - `undefined` for the root, where Node's `JSON.stringify` and `JSON.parse` return
    `undefined`, throws a `TypeError` that says so (Velt's results are a `string` and a
    `JsonValue`), rather than writing `"null"`.
  - When #924 lands (its `any` backed by this design's tape), both take `any` as in TypeScript
    (`typeof v === "bigint"`, `this` as the holder), the unannotated form compiles with Node's
    meaning, and `JSON.parse(text, reviver)` works as written in TypeScript. The `JsonValue`
    forms stay.

### 12. Schema and the low-level API

- **`JSON.schema<T>(): string`** returns the JSON Schema (draft 2020-12) of what
  `JSON.parse<T>` accepts. It includes required keys, literal unions (`enum`), discriminators
  (`oneOf` with `const`), decorators and `unknownKeys`. Sema builds it from the same contract as
  the decoder, so the two cannot drift apart; the result is a string constant.
- **`json.Reader` and `json.Writer`** are public in `velt/encoding/json`. They are the pull reader and
  the builder the glue already uses, and are the types converters receive. With them, hand-made
  parsers can run at the speed of the generated ones, as STJ's `Utf8JsonReader` does.

## Order of work

| Step | What | Changes users see | Depends on |
|---|---|---|---|
| 0 | Reader, scanner and writer in Velt | None | — |
| 1 | Key matching in object decoders | None (faster) | 0 |
| 2 | Benchmark corpora and new comparison rows | None | — |
| 3 | SIMD scanner, SWAR numbers | None (faster) | 0, 2 |
| 4 | Array capacity hints | None (faster) | — |
| 5 | Tape `JsonValue` | None (faster) | 0, 2 |
| 6 | Parse from bytes; `req.json` / `res.json` without a string | New overloads | 3 |
| 7 | `JSON.stringifyTo` | New API | — |
| 8 | `parseStream`, `parseLines` | New API | 6 |
| 9 | Compile-time decorators | New syntax | contract sign-off |
| 10 | `@json.*` | New API | 9; converters use the internal `Reader` and `Writer` of step 0 (step 12 makes them public) |
| 11 | JSONC; reviver and replacer functions | New options | 5 |
| 12 | `JSON.schema<T>()`, public `Reader`/`Writer` | New API | — |

Each step is one pull request:
- a step that changes speed has `bench/json/run.sh` numbers before and after, plus build time
  and IR size, with every row within 3% or better;
- a step that users see updates `docs/std/json.md` (marked **Planned** until it lands) and adds
  goldens.

## Open questions

1. Interfaces and type aliases cannot carry decorators. Is "use a class or struct" enough, or do
   we also want `JSON.codec<T>({ naming: "snake_case", … })` as a value that holds the same
   options for a type it cannot decorate?
2. Should `@json.naming` also be a program-wide setting in the manifest? STJ has
   `JsonSerializerOptions` defaults. A setting in the manifest is convenient, but it would let
   a dependency's JSON shape depend on the program that imports it. The proposal is per class
   only.
3. Step 5: should `JsonValue` keep the source alive (zero copy, but a small value can pin a large
   document) or copy the strings it holds when the document is large and a value outlives it?
   STJ's `JsonElement.Clone()` makes this explicit. A `value.detach()` would be the Velt
   equivalent.
4. Should `JSON.parse<T>` reject duplicate keys under an option? STJ gained
   `AllowDuplicateProperties = false` in .NET 9.
