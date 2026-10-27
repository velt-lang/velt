# Design: the JSON engine, fastest in class with System.Text.Json's features

Status: proposed. Nothing here is implemented beyond what [velt:json](../../std/json.md)
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
and byte views, `velt:simd`, integer types, classes and structs). This includes the scanner,
the pull reader, number parsing and printing, string escaping, `JsonValue` and the writer. It
is the rule `BTreeMap` follows (#1543). No logic goes into new Rust code in `velt_rt` or
`velt_text`, and no Rust crate is used. The reasons:

- **It proves the claim.** If `velt:json` is Velt and beats serde_json, the language is fast.
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

### 0. Move the reader, scanner and writer into Velt (no API change)

Port `velt_text/src/json_scan.rs`, `velt_rt/src/json/reader.rs`, `json_escape.rs`, the JSON
pushes of `strbuf.rs` and float printing (`ryu`, as a std port) to Velt in
`std/prelude/json/`. Then switch the glue in `crates/velt_vir/src/lower/json/` from
`Rt::Json*` calls to calls of those Velt methods.

Error messages and paths stay the same, and the golden tests do not change. Node's exact
syntax errors (`node_syntax.rs`) can stay a Rust slow path for now, as they run only after a
failure. The database row readers and native packages keep their own Rust copy until each is
ported. The Rust JSON code is deleted once nothing calls it.

**Gate:** each JSON benchmark at most 3% slower than today's Rust engine. Every gap is
measured, filed and fixed in the compiler before moving on.

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

**Cost.** The code per decoder grows by about one word compare per field, and there is no
runtime cost. The PR reports the change in build time and IR size.

### 2. Benchmarks that others publish

Add the corpora every JSON library reports on to `bench/json`:
- `twitter.json`: mixed, with non-ASCII strings;
- `citm_catalog.json`: wide objects and many integers;
- `canada.json`: floats.

`bench/json/fetch.sh` downloads them with pinned SHA-256s; they are not committed. Each corpus
runs as a typed parse, a `parseValue`, and a stringify of the result. The comparison rows are
serde_json, sonic-rs and simd-json (Rust), Node and Bun.

Targets:
- typed parse faster than serde_json on all three corpora, and at sonic-rs's speed on
  `citm_catalog`;
- `parseValue` and `navigate` faster than Node;
- stringify faster than serde_json.

### 3. A SIMD scanner and SWAR numbers

The scanner from step 0 is written with `velt:simd` (`u8x16`, compare and movemask), so the
same Velt source becomes NEON or SSE2 code. A missing vector operation is added to `velt:simd`
and the backends, not written in Rust:
- **The inside of a string.** It looks 16 bytes at a time (16-byte vectors) for `"`, `\`,
  bytes below 0x20 and bytes of 0x80 or more. While doing so it counts UTF-16 units, so the
  table lookup runs only on blocks that are not ASCII. This follows the shape of
  `json_escape.rs`'s `clean_run`.
- **Whitespace.** A fast exit when the next byte is not whitespace (the common case in compact
  JSON), and a 16-byte skip for indented JSON.
- **Numbers.** Eight digits are parsed at once with SWAR into the `u64` accumulator. The result
  is exact up to 19 digits, and a mantissa and exponent that fit go through Clinger's fast path.
  Only the rest falls back to `str::parse`.


### 4. Arrays

A decoded `T[]` grows by doubling, as today, but starts at the capacity the last array decoded
at the same depth by the same decoder ended with. That is a hint kept in the reader's context
and costs nothing to keep. It avoids most regrowth on documents of many similar arrays without
scanning ahead.

### 5. A tape for `JsonValue`

`JSON.parseValue` writes a **tape**, a Velt class in `velt:json`: one `u64[]` of entries. Each entry is a 4-bit kind, a
28-bit length, and a 32-bit offset into the source or into the tape. The source string is kept
alive by one reference. A container's entry holds the index of its end, so skipping a subtree is
O(1). Strings without escapes are byte ranges of the source; strings with escapes are decoded
into a side buffer when the tape is built. An object with more than 16 keys gets its key index
built lazily, on the first lookup by key.

A `JsonValue` becomes `(tape: Tape, index: u32)`, a Velt struct:
- `get(key)` and `at(i)` return a new handle into the same tape: no allocation, and no reference
  count except for the tape's;
- `asString()` copies a slice of the source;
- edits turn the touched path into Velt nodes (copy-on-write) and leave the rest on the tape.

The Rust `JsonValue` (`velt_rt/src/json/value*.rs`) and its ABI in
`docs/internals/contracts/rt_abi_async.md` §12 go away once nothing calls them; the methods
users see do not change. This is STJ's split between `JsonDocument`/`JsonElement` (read-only, flat) and `JsonNode`
(mutable), with Velt keeping one type in front of both.

### 6. Parsing bytes

The new overloads parse a `Uint8Array`, an `ArrayBuffer` or a `u8[]` as UTF-8 directly:
- `JSON.parse<T>(bytes, options?)`
- `JSON.parseValue(bytes)`

The scanner already works on bytes. It adds a UTF-8 check that runs inside the string scan
(non-ASCII blocks only), and invalid UTF-8 is a `JsonError` that gives the byte offset. Changes
in std:
- `Request.json<T>()` and `Response.json<T>()` parse the body bytes, without making a `string`;
- `velt:fs`'s `readFile` paired with `JSON.parse<T>` documents the pattern.

```ts ignore
const cfg = JSON.parse<Config>(await readFile("config.json")); // bytes in, no string made
```

### 7. Writing to a sink

`JSON.stringifyTo(out, value)` writes into a byte sink without making a `string`. The sink is a
`WritableStream<Uint8Array>`, a socket, a file, or a `velt:io` writer. The writer glue already
appends to a `strbuf`; the sink's buffer drains whenever it passes 64 KiB, as STJ's
`Utf8JsonWriter` does over a `PipeWriter`. `Response.json(data)` streams the same way when the
body is large.

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
import { json } from "velt:json";

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

The decorators live in `velt:json`'s `json` namespace and are checked by sema.

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
- **`JSON.parse(text, reviver)` and `JSON.stringify(x, replacerFn, space)` work as in
  JavaScript, on the dynamic path.**
  - The reviver form returns a `JsonValue`; the replacer function receives
    `(key: string, value: Value)`. Typed parsing does not take a reviver, because a reviver can
    change a value's type, which a static type cannot express. The diagnostic suggests a
    converter.
  - The replacer function replaces today's compile error.

### 12. Schema and the low-level API

- **`JSON.schema<T>(): string`** returns the JSON Schema (draft 2020-12) of what
  `JSON.parse<T>` accepts. It includes required keys, literal unions (`enum`), discriminators
  (`oneOf` with `const`), decorators and `unknownKeys`. Sema builds it from the same contract as
  the decoder, so the two cannot drift apart; the result is a string constant.
- **`json.Reader` and `json.Writer`** are public in `velt:json`. They are the pull reader and
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
| 7 | `JSON.stringifyTo`, streamed `Response.json` | New API | — |
| 8 | `parseStream`, `parseLines` | New API | 6 |
| 9 | Compile-time decorators | New syntax | contract sign-off |
| 10 | `@json.*` | New API | 9, 12's `Reader`/`Writer` for converters |
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
