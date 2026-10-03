# Design: JavaScript string semantics (UTF-16 code units)

Status: decided (issue #377, from #326), revised after the design review on #377. The decisions
are listed at the end. Phase 1 (the representation) is implemented: every string carries its
UTF-16 unit count (`w1` = units|bytes, the inline non-ASCII form, the header on non-ASCII heap
buffers, `push_wtf8`), with no visible change. Every length and position still counts **bytes**
until phase 2 ([types](../../reference/types.md#strings),
[rt_abi.md "Strings"](../contracts/rt_abi.md)).

## Problem

TypeScript strings are sequences of UTF-16 code units. `length`, `slice`, `indexOf`,
`charCodeAt`, `padStart`, `split("")`, regex offsets and `<` all count or compare code units.
Velt counts UTF-8 bytes, and the two agree only for ASCII:

```ts ignore
"héllo".length;              // Velt 6, TypeScript 5
"héllo".slice(0, 2);         // Velt "h", TypeScript "hé"
"Zoë".length > 3;            // Velt true, TypeScript false
"€".charCodeAt(0);           // Velt 226 (a byte), TypeScript 8364
"😀".length;                 // Velt 4, TypeScript 2
"～" < "\u{1F600}";      // Velt true (code point order), TypeScript false
```

Nothing warns: the program compiles and gives a different answer. That was acceptable as a
documented difference, but it fails the goal set in #326, where the same source file compiles
with `tsc` and with `velt` and behaves the same in both. A validator shared between a browser and
a Velt server (`name.length <= 3`) accepts a name on one side and rejects it on the other.
`s[i]` and `for (const c of s)` are rejected outright.

The goal is **exactly TypeScript's string semantics** with three constraints:

- ASCII text keeps today's speed (every benchmark within 3%);
- I/O stays zero-copy (files, sockets, HTTP, JSON, native packages and the regex engine all speak
  UTF-8);
- non-ASCII text pays a bounded, predictable cost, never O(n) per index.

## Proposal

### Semantics

A Velt `string` **is** a sequence of UTF-16 code units, as in JavaScript. Every API that counts
or indexes counts code units:

| API | Meaning (as in JS) |
|---|---|
| `s.length` | number of UTF-16 code units (`"😀".length` is 2) |
| `slice`, `substring`, `indexOf`, `lastIndexOf`, `includes`, `startsWith`, `endsWith`, `padStart`, `padEnd`, `at`, `charAt`, `charCodeAt`, `codePointAt` | positions and lengths in code units |
| `s[i]` | the code unit at `i` as a one-unit string; `""` out of range (decision 3) |
| `for (const c of s)`, `[...s]`, `Array.from(s)` | code points (a surrogate pair is one element), as JS's string iterator |
| `split("")`, `replaceAll("", x)` | code units (a pair splits into two lone surrogates), as JS |
| `String.fromCharCode(...units)`, `String.fromCodePoint(...cps)` | as JS; `fromCodePoint(0xD83D, 0xDE00) === "😀"` |
| `<`, `<=`, `>`, `>=`, `sort()` without a comparator | code-unit order |
| `==`, `Map`/`Set` keys | same code units |
| `isWellFormed()`, `toWellFormed()` | ES2024 |
| regex `index`, `end` and `exec(s, from)` | code units |
| `localeCompare` | unchanged (CLDR root collation) |

Lone surrogates are ordinary string contents, as in JS: `"😀".slice(0, 1)` is the one-unit string
`"\uD83D"`, and gluing the two halves back together gives `"😀"` again. Every search operation can
match half of a pair:

- `"😀".indexOf("\uDE00")` is 1, and `"😀".lastIndexOf("\uD83D")` is 0;
- `"a😀b".split("\uDE00")` is `["a\ud83d", "b"]`;
- `"😀".replace("\uDE00", "X")` is `"\ud83dX"`;
- `"😀".replaceAll("", "-")` is `"-\ud83d-\ude00-"`;
- positions inside a pair: `"😀".startsWith("\uDE00", 1)`, `"😀".endsWith("\uD83D", 1)` and
  `"😀".indexOf("", 1) === 1`.

The **type** of `length` and of position parameters is not decided here: it follows whatever #214
decides for `Array.length` (`usize` today). This design only changes the unit.

Byte counts and byte views keep their JavaScript names:

- `Buffer.byteLength(s)`, the UTF-8 length, is O(1). With an encoding argument it follows Node:
  `utf16le` is `2 × length`, and `base64` is the decoded size.
- `new TextEncoder().encode(s)` and `Buffer.from(s)`: a zero-copy view of a well-formed string's
  bytes (Node copies), so byte-level parsers over `u8[]` run at full speed and still run in Node.
  `encodeInto` never splits a code point, and its `read` counts code units (`"é😀"` into 5 bytes
  gives `{ read: 1, written: 2 }`).
- `new TextDecoder(label, options).decode(bytes, { stream })`, Node-exact: lossy by default; one
  leading BOM stripped unless `ignoreBOM`; `fatal: true` throws `TypeError`; `stream: true`
  carries a split sequence across calls; labels follow the Encoding Standard (`"latin1"` is
  windows-1252), and an unknown label throws `RangeError`.

There is no Velt-only byte-offset string API. A `velt:bytes` module waits until a benchmark shows
a gap that the standard APIs above can't close; even then `length` keeps one meaning.

### Representation: WTF-8 with the UTF-16 length in the value

The bytes stay UTF-8, so I/O stays zero-copy.

**1. Canonical WTF-8.** UTF-8 can't encode lone surrogates. WTF-8 (the encoding Rust uses for
`OsString` on Windows) is UTF-8 that also allows a surrogate code point as a 3-byte sequence
(`ED A0..BF xx`). It holds every UTF-16 sequence. Canonical form: a surrogate *pair* is always
stored as its 4-byte code point, never as two 3-byte halves. Because of that rule, byte equality
is code-unit equality, and hashing the bytes stays correct (`search.rs` `str_eq`, `hash.rs`).

**2. The unit count is in the value.** For heap and static strings, `w1` packs
`units:32 | bytes:32` (units in the high half). Every non-ASCII character has more bytes than
units, so **a string is ASCII exactly when `units == bytes`**; no flag bit is needed.

| Form | Byte length | Unit length | ASCII test |
|---|---|---|---|
| static / borrowed, heap | low half of `w1` | high half of `w1` (`w1 >> 32`) | `units == bytes` |
| inline, ASCII (≤ 23 bytes) | byte 23 = `0x80 \| len` | same as the byte length | bit 0x40 of byte 23 clear |
| inline, non-ASCII (≤ 22 bytes) | byte 23 = `0xC0 \| len` | byte 22 | bit 0x40 of byte 23 set |

- The `length` read stays branch-free: two selects over byte 23 and `w1 >> 32`
  (`velt_vir/src/lower/strings.rs`).
- A string is limited to 4 GiB of bytes. Node caps strings at 2²⁹ − 24 units
  (`MAX_STRING_LENGTH`), so no program that works on Node loses. The producers that can reach the
  limit (file reads, HTTP bodies, child output, stdin, the builder) report "string too long",
  checked once in `heap::layout`.
- A **borrowed** non-ASCII view (a `split` piece of a literal, a JSON key pointing into the parsed
  text) keeps its unit count in `w1`, with no copy. Only a view of 64 units or more that gets
  indexed is copied, to get breadcrumbs.

**3. A header only on non-ASCII heap buffers.** ASCII buffers keep today's layout,
`[count][bytes]`. A non-ASCII buffer is `[crumbs: atomic ptr][lone: u64][count][bytes]`. The count
stays at `ptr - 8`, so retain is unchanged; release, `heap::grow` and free take the header size
from the value (`units != bytes`).

- `lone` counts the lone surrogates. It is what decides whether a seam can join and whether
  output needs conversion; an exact boolean can't survive a join, a count can:
  `lone(a + b) = lone(a) + lone(b) − 2 · joined`.
- An ASCII buffer moves to a buffer with a header **at the push that writes its first byte
  ≥ 0x80**, not when a builder finishes: template lowering and `s += x` use the live builder
  value.
- Long non-ASCII **literals** are emitted in the heap form, in writable data, with an immortal
  count, so publishing crumbs (a compare-and-swap) never writes read-only memory. Short ones are
  inline or static, and a static string has no header to write.
- The layout leaves room for a later shared-slice form `{ptr into buffer, len, owner}` (see
  "Allocations").

**4. One way in: `push_wtf8`.** `VeltStr::push_wtf8(&mut self, bytes, summary: Option<Summary>)`
is the only way bytes enter a string. It updates the unit count, the lone count and the layout,
and joins a trailing high surrogate to a leading low one only when both sides have lone
surrogates. `push_bytes`, `from_bytes`, `from_vec` and `append_unique` become private to `str/`.

Joining a high and a low half keeps the **unit count unchanged** and shortens the bytes by 2.
The producers that can glue halves together, and so must go through `push_wtf8`:

- `str_concat` and the builder (`push_str`/`push_bytes`, which also carries templates,
  `Array.join` and `s += x`);
- `repeat`, `padStart`/`padEnd` (fill against fill, and fill against the string);
- `replace`/`replaceAll` (two seams per match) and regex `replace`;
- JSON escape decoding (`"😀"` is `"😀"`) and `json/value_edit`;
- `String.fromCharCode`/`fromCodePoint` and UTF-16 decoders (`TextDecoder("utf-16le")`);
- on the compiler side: the lexer, constant and template folding, and JSX precompile.

What must be exact is the unit count, the layout and canonical bytes; everything else may be
conservative. The **debug runtime** recounts every produced string and checks its canonical
form.

**5. Breadcrumbs.** For random access into long non-ASCII text (Swift's `String` uses the same
technique for its UTF-16 view):

- a table of the byte offset of every 64th code unit, built on the first index operation on a
  string of more than 64 units;
- published with a compare-and-swap, because strings cross threads;
- part of the string's lifetime: freed with the buffer.

An index lookup is then one table load plus a forward scan of at most 63 units. The table costs
about 1/16 of the text's size (one `u32` per 64 units), and only for non-ASCII strings that are
indexed. Crumbs stay valid for the prefix when a uniquely owned buffer is appended to, and are
extended lazily.

### How the operations compile

| Operation | ASCII (`units == bytes`) | Non-ASCII |
|---|---|---|
| `length` | inline select, as today | the same select |
| `charCodeAt(i)`, `s[i]`, `at`, `charAt` | inline bounds check plus byte load, as today | the per-local cursor (below), else breadcrumbs, then a decode; a 4-byte sequence gives its high or low surrogate |
| `slice`, `substring` | byte offsets, as today | translate both ends; an end between two halves of a pair re-encodes that half as a 3-byte lone surrogate (a copy) |
| `indexOf`, `lastIndexOf`, `includes`, `startsWith`, `endsWith`, `split`, `replace`, `replaceAll` | byte search, as today | byte search on WTF-8 (self-synchronizing), then translate the found offset; a needle that starts with a lone low surrogate or ends with a lone high one, a position inside a pair, and the empty needle take a code-unit path |
| `<`, `sort()` | `memcmp`, as today | the ordering rule below |
| `==`, hash | unchanged | unchanged (canonical WTF-8) |
| `+`, template literals | unchanged, plus the unit sum | plus a join check when both parts have lone surrogates |
| `toUpperCase`, `toLowerCase` | as today | as today; the layout comes from the output (`"ſ".toUpperCase()` is `"S"`) |
| `for...of` | byte loop | a straight decode loop (lone surrogates decode as themselves) |

**The ordering rule.** Byte order is code-point order. It differs from code-unit order between
U+E000–U+FFFF and supplementary characters, and also between lone surrogates and supplementary
characters (`"\uDC00" > "\u{10000}"` is `true` in Node). With `memcmp` as the fast path:

1. find the first differing byte, then step back to the start of that code point (the prefix is
   shared, so it is the same position in both strings);
2. compare `firstUnit(cp)` of each side: `cp` below 0x10000, else
   `0xD800 + ((cp − 0x10000) >> 10)`;
3. if they are equal (two supplementary characters with the same high surrogate, or one of them
   a lone high surrogate), compare the pair's low surrogate with the other side's next first
   unit, or with the end of the string (the shorter side is less). In canonical form one extra
   comparison is always enough.

A byte prefix is a unit prefix. Checked on 3,000,000 random pairs against a `Vec<u16>` model with
no wrong results (the rule in the first version of this note had 16,835, plain `memcmp`
31,617). The VIR interpreter's `str_cmp` uses the same rule.

**Sequential indexing.** Breadcrumbs alone are 15–50× slower than Node on a
`for (i < s.length) s.charCodeAt(i)` loop over non-ASCII text (measured in the review). So:

- **A per-local cursor slot in the lowering** (phase 2): each string local that is indexed gets a
  `(unit index, byte offset)` cursor, reset on every assignment to the local. An index near the
  cursor steps from it; a one-unit step is the fast path. The cursor format has a half-unit bit
  for a position inside a pair. A cursor in the string header would bounce the reference count's
  cache line between cores scanning one shared string, so it lives with the local.
- **Strength reduction** (phase 3): `for (i < s.length) … s.charCodeAt(i) / s[i]` becomes a walk
  that advances one unit per step.

ns per `charCodeAt` over 1M-unit strings, from the review's prototype (a loaded machine, so
compare ratios):

| text | crumbs | cursor | strength-reduced | Node sequential | crumbs random | Node random |
|---|---|---|---|---|---|---|
| ASCII content | 31–38 | 2.2–2.9 | 0.4–0.6 | 1.5–1.6 | 63–67 | 3.1–3.6 |
| 5 % two-byte | 28–34 | 3.1–3.9 | 1.2–1.3 | 1.6–1.9 | 67–77 | 5.5–7.1 |
| CJK | 64–72 | 5.0–5.1 | 1.9–2.1 | 2.0–2.2 | 109–126 | 5.3–7.0 |
| emoji-heavy | 73–75 | 6.7–8.0 | 4.0–4.6 | 1.7–2.2 | 113–130 | 11–14 |

("ASCII content" is ASCII text stored in a non-ASCII string; pure ASCII strings take the inline
byte load, about 0.5 ns.)

### Boundaries

Text enters and leaves the program as UTF-8, as today. Only text with lone surrogates
(`lone > 0`) needs work at a boundary. All of it lands in the same PR that first lets lone
surrogates exist (phase 2): before that, a lone surrogate reaching a Rust `&str` built with
`from_utf8_unchecked` is undefined behaviour.

- **The runtime's view.** `text()` (`str_ops/mod.rs`, used by case mapping, number parsing,
  replace, search, slice, split and collation) returns `Result<&str, Wtf8>`; the same goes for
  `json/value.rs` `owned_text` and the sqlite bindings. One `wtf8_to_utf8_lossy` maps each lone
  surrogate to exactly **one** U+FFFD (Rust's `from_utf8_lossy` would give three). Every output
  and every lossy site uses it, so `Buffer.byteLength(s)` (the stored byte length) is also the
  output length: a lone surrogate and U+FFFD both take 3 bytes.

| Boundary | Behaviour |
|---|---|
| decoding (files, sockets, HTTP, stdin, databases) | strict or lossy UTF-8 as today. Surrogate encodings in external input (`ED A0..BF xx`) are **invalid UTF-8**: strict mode refuses them, lossy mode replaces them, and they are never joined (Node decodes `ED A0 BD ED B8 80` as six U+FFFD). Decoding also counts units. |
| OS input (arguments, environment, paths) | lossy, both ways, as Node. On Windows `OsString` is WTF-8 inside, so a zero-copy path would keep lone surrogates; it must convert. |
| writing (files, sockets, HTTP bodies, process arguments, environment) | well-formed text is written as is (zero-copy); a lone surrogate becomes U+FFFD |
| `console.log` | a top-level string prints its lone surrogates as U+FFFD; a string nested in an array or object is escaped as `inspect` does (`console.log(["\uD83D"])` prints `[ '\ud83d' ]`), and so is `%o` |
| native packages | `str_of` returns a `Cow`: well-formed text is borrowed as today, ill-formed text is a converted copy (no ABI bump; old bundles stay fatal on ill-formed text). `str_new` scans and canonicalises its input with the decoding rule above. |
| `JSON.parse` | `"\ud800"` escapes keep the lone surrogate, and an escaped pair joins |
| `JSON.stringify` | a lone surrogate is written as a `\udXXX` escape (ES2019 well-formed `JSON.stringify`) |
| `encodeURIComponent`, `decodeURIComponent` | a lone surrogate, or `%ED%A0%BD`, throws `URIError`; `URL` and `URLSearchParams` write `%EF%BF%BD`. `std/url` never accepts surrogate bytes. |
| string literals | `"\uD83D"`, `"\u{D83D}"` and `"😀"` (stored as the joined 4-byte code point) are accepted, as in JS |

**Regex.** In Unicode mode, `regex::bytes` never matches the bytes of a lone surrogate (its UTF-8
sequences leave out D800–DFFF). This design does the minimum: `.` and negated classes (`[^…]`,
`\S`, `\W`, `\D`) accept lone surrogates through hand-built byte classes, and offsets are code
units. JavaScript's regex syntax and semantics (`u` and non-`u` modes, Annex B escapes,
JavaScript's `\s`, case folding per mode, empty-match stepping, `d`-flag indices) are #401:
today's translator is neither JavaScript's `u` mode nor its default mode.

### Allocations

- ASCII strings allocate exactly as today.
- `s[i]`, `charAt` and `for...of` items are inline strings (no allocation).
- The header is part of the string's own allocation.
- Breadcrumbs allocate once per long non-ASCII string that gets indexed.
- In-place append (`s += x` on a uniquely owned string, #381) stays O(1): units add (a join
  doesn't change them), the layout moves once to a buffer with a header, the lone count follows
  the formula above, crumbs are extended lazily, and a cursor is reset on a join.
- Not changed by this design: a heap `slice` copies (`str/mod.rs` `substring`), so
  `rest = rest.slice(n)` parsers are O(n²), where V8 uses sliced strings. That is #402; the
  header layout must not prevent a later shared-slice form.

## Cost

- **ASCII text:** the `length` select reads `w1 >> 32` instead of `w1`; `charCodeAt` tests
  `units == bytes`; concatenation adds unit counts. `bench/strings` (24 MB of ASCII scanned with
  `charCodeAt`) must stay within the 3% gate.
- **Heap strings:** ASCII buffers are unchanged; non-ASCII buffers grow by 16 bytes.
  `bench/hashmap` and `bench/sort` cover the common paths.
- **Non-ASCII text:** `length` is a load. Sequential index loops reach Node's time through the
  cursor and strength reduction. Random indexing of large non-ASCII strings stays about 10×
  slower than Node (a breadcrumb lookup plus a scan); the docs say so.
- **I/O** is unchanged for well-formed text. Text with lone surrogates (rare: only produced by
  slicing between the halves of a pair, by `fromCharCode` or by JSON escapes) is converted when
  written.
- `bench/strings_utf16` measures a `charCodeAt` loop over non-ASCII text, a sort of non-ASCII
  keys, split and join of non-ASCII CSV, JSON with emoji and an append loop, each against Node.

## Diagnostics and migration

Most code needs no change: positions from `indexOf` passed to `slice` stay consistent, and ASCII
text gives the same results as before. Code that used `length` as a **byte count** changes
meaning silently. That covers buffer sizes, a `Content-Length` computed in Velt, and wire
protocols. The migration note in the release says so and points to `Buffer.byteLength(s)`.

Diagnostics that go away:
- ``cannot index a value of type `string` ``;
- ``cannot iterate over a value of type `string` ``.

New diagnostics: none. The semantics are TypeScript's, so there is nothing to explain.

Docs that change: types.md "Strings", prelude.md, std/README.md, regex.md, json.md (lone
surrogates), encoding.md (`TextEncoder`), and ts-developers.md, which loses its row and
paragraph on byte lengths. The `ts` blocks that print a length (types.md `label`) get non-ASCII
examples, so the docs test covers the new rule.

### Code in the repository that assumes byte offsets

- `std/csv.vlt` scans a `u8[]` and slices the string with those byte positions. It changes to
  scan with `charCodeAt` (the delimiter is ASCII, so the ASCII fast path applies).
- `std/regex.vlt` forwards the runtime's offsets. The runtime translates them.
- `std/cli.vlt`: help column widths now count code units, which aligns better. Short-flag
  clusters split per code unit, as in Node.
- `std/url`: byte work moves to `u8[]` (`utf8Encode`), string positions stay positions, and
  surrogate bytes are refused.
- `std/uuid.vlt`, `std/path.vlt` and `std/datetime`: ASCII positions are unchanged.
- Runtime functions that take or return positions (`str_ops/*`, the regex and the JSON value
  length) convert at their entry and exit.
- Golden tests:
  - `lang/json_duplicate_keys` expects `lone.length` to be 4 (the lone surrogates are kept),
    not 8;
  - new tests cover `length`, `slice`, `indexOf`, `charCodeAt`, `s[i]`, `for...of`, ordering
    and regex offsets on non-ASCII and ill-formed text.
- `tests/difftest` drops its ASCII-only restriction for strings and regex subjects; differential
  testing against Node gates phase 2.
- `velt_lsp` already converts compiler byte offsets to UTF-16 columns. Compiler source positions
  stay byte offsets, so it is unaffected.

### Contract changes

- `docs/internals/contracts/rt_abi.md` "Strings": `w1` as units|bytes, the inline non-ASCII
  form, the header on non-ASCII buffers and its invariants, canonical WTF-8, `str_cmp` in
  code-unit order.
- `rt_abi_async.md` §12.2: the "POC indexing model" paragraph is replaced by code units; the
  method table gains the new functions.
- `native_abi.md`: `str_new` scans and canonicalises; the SDK's `str_of` returns a `Cow`.
- AST and HIR: `Lit::Str(String)` (`ast.rs`, `hir`) can't hold `"\uD83D"`. String literals,
  literal types, string-enum values, `switch` cases, record keys and JSX folding carry WTF-8
  bytes instead.
- `vir.rs`: `STR_AGG` stays three words. Lowering of string literals packs `w1` and emits long
  non-ASCII literals in the heap form; the inline `length` and `charCodeAt` sequences change as
  above.

## Alternatives considered

1. **Store UTF-16**, as Java, C# and JavaScript engines do. Indexing is O(1) with no tables, but
   every boundary converts: files, sockets, HTTP, JSON, the UTF-8-only regex engine and native
   packages. ASCII memory doubles. Rejected because the boundaries are where a server spends its
   time.
2. **Latin-1 or UTF-16 per string**, as V8 does. Pure ASCII still writes out without a copy, but
   Latin-1 text such as `"é"` converts at every boundary, the regex engine needs a converted copy
   of two-byte text, and every runtime function has two code paths. This is the closest
   competitor. WTF-8 plus a cursor and breadcrumbs gets the same ASCII speed with one code path
   and zero-copy I/O for all well-formed text.
3. **Keep bytes and document the difference** (today). Rejected: it blocks sharing code with a
   TypeScript client (#326), and it is a silent divergence.
4. **Code-point indexing** (Python 3). It is neither JavaScript's rule nor cheaper.
5. **A per-module or per-project switch.** Rejected: it would give two meanings for `length`,
   against "one way of doing things".
6. **A flag bit plus a UTF-16 length in a header** (the first version of this note). The unit
   count in `w1` makes `length` a plain select for every form, needs no header on ASCII buffers,
   and lets borrowed non-ASCII views keep their length without a copy.

## Implementation order

One PR each:

0. Tests and benchmarks first: a `Vec<u16>` reference model of the string operations, checked
   against tables generated by Node, and a property test of the runtime against the model (ASCII
   only until phase 2; ASCII, BMP, astral and lone-surrogate text with lengths around 22, 23, 64
   and 128 units after); `bench/strings_utf16` with Node baselines.
1. Representation (done): `w1` as units|bytes, the inline non-ASCII form, the header on
   non-ASCII buffers only, `push_wtf8` as the only producer, the lone count, `str_new` scanning,
   the contract updates (rt_abi, native_abi) and the debug-runtime invariant check. No visible
   change. Moved to later phases because they only matter once lone surrogates exist (phase 2)
   or can be written (phase 4): `text()` returning `Result<&str, Wtf8>` and the lossy conversion
   at one U+FFFD per surrogate, the AST/HIR `Lit::Str` contract change.
2. Semantics, with every boundary: code-unit positions, the ordering rule, the half-pair paths,
   U+FFFD and `inspect` escaping on output, lossy OS input, JSON `\udXXX`, the native `Cow`, the
   per-local cursor, the std migration (`csv`, `url`, `cli`), `Buffer.byteLength`, and difftest
   over non-ASCII text. From phase 1: `text()` returning `Result<&str, Wtf8>` with one U+FFFD per
   lone surrogate; breadcrumbs (the header's `crumbs` field is reserved and null); long
   non-ASCII literals in the heap form (literals stay static until crumbs need a writable
   header); and the producers that build a result in a `Vec` before making it a string
   (`replace`, `repeat`, `padStart`/`padEnd`, JSON decoding) push their pieces through
   `push_wtf8` so their seams join (they can't meet a lone surrogate before this phase).
3. Strength reduction of index loops, and `for...of` as a decode loop.
4. New APIs: `s[i]`, `at`, `charAt`, `codePointAt`, `String.fromCodePoint`, variadic
   `fromCharCode`, `isWellFormed`/`toWellFormed`, position arguments for
   `includes`/`startsWith`/`endsWith`, Node-exact `TextEncoder`/`TextDecoder` with `encodeInto`
   and a zero-copy `encode`, and literal escapes for lone surrogates and joined pairs, with the
   AST/HIR `Lit::Str` contract change they need (from phase 1).
5. The regex minimum: lone surrogates in `.` and negated classes, and code-unit offsets. The rest
   goes to #401.

## Decisions

1. A `string` is a sequence of UTF-16 code units, exactly as in JavaScript, with no per-module or
   per-project switch: one meaning of `length`.
2. Storage is canonical WTF-8, with the unit count in the value (`w1` = units|bytes, byte 22 for
   inline non-ASCII strings) and a header only on non-ASCII heap buffers.
3. `s[i]` out of range is `""`. JavaScript returns `undefined`, which TypeScript types as
   `string`; idioms such as `while (t[i] === " ") i++` read past the end on purpose, and `""`
   compares with non-empty literals as `undefined` does (`=== undefined` is already a Velt error
   with a fix). `s.at(i)` returns `string | null`.
4. `charCodeAt` out of range keeps returning -1 for now; `NaN` belongs to the number-semantics
   decision in #214, not to this design.
5. Regex: this design does the minimum (lone surrogates in `.` and negated classes, code-unit
   offsets); JavaScript regex syntax and semantics are #401.
6. The cursor lands with the semantics (phase 2) and strength reduction right after (phase 3),
   not "later".
7. The type of `length` and of positions follows #214's decision for `Array.length`.
