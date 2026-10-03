# Design: JavaScript string semantics (UTF-16 code units)

Status: decided (issue #377, from #326); the decisions are listed at the end. Nothing here is
implemented yet. Today a string is UTF-8, and every
length and position counts **bytes** ([types](../../reference/types.md#strings),
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
| `s[i]` | the code unit at `i` as a one-unit string; out of range panics `index out of bounds`, like arrays |
| `for (const c of s)`, `[...s]`, `Array.from(s)` | code points (a surrogate pair is one element), as JS's string iterator |
| `split("")` | code units (a pair splits into two lone surrogates), as JS |
| `String.fromCharCode(...units)`, `String.fromCodePoint(...cps)` | as JS |
| `<`, `<=`, `>`, `>=`, `sort()` without a comparator | code-unit order |
| `==`, `Map`/`Set` keys | same code units |
| `isWellFormed()`, `toWellFormed()` | ES2024 |
| regex `index`, `end` and `exec(s, from)` | code units |
| `localeCompare` | unchanged (CLDR root collation) |

Lone surrogates are ordinary string contents, as in JS: `"😀".slice(0, 1)` is the one-unit string
`"\uD83D"`, and gluing the two halves back together gives `"😀"` again.

The **type** of `length` and of position parameters is not decided here: it follows whatever #214
decides for `Array.length` (`usize` today). This design only changes the unit.

Byte counts remain available, under their JavaScript names:

- `Buffer.byteLength(s)`, the UTF-8 length, is O(1);
- `new TextEncoder().encode(s)` and `new TextDecoder().decode(bytes)` (as thin classes over
  today's `utf8Encode` and `utf8Decode`/`utf8DecodeLossy` in `velt:encoding`).

### Representation: WTF-8 plus a cached UTF-16 view

The bytes stay UTF-8, so I/O stays zero-copy. Three additions make UTF-16 indexing cheap.

**1. WTF-8 storage.** UTF-8 can't encode lone surrogates. WTF-8 (the encoding Rust uses for
`OsString` on Windows) is UTF-8 that also allows a surrogate code point as a 3-byte sequence
(`ED A0..BF xx`). It holds every UTF-16 sequence. Canonical form: a surrogate *pair* is always
stored as its 4-byte code point, never as two 3-byte halves. Because of that rule, byte equality
is code-unit equality, and hashing the bytes stays correct.

Concatenation and the builder join halves where they meet: a left part ending in a high
surrogate plus a right part starting with a low one become one 4-byte sequence. The check runs
only when both parts are flagged ill-formed (below), so normal text never pays for it.

**2. Flags in the value.** One bit says the text has a byte ≥ 0x80 (`NON_ASCII`). The all-zero
value stays the empty ASCII string. For ASCII text a code unit is a byte, so every operation is
exactly today's code.

| Form | Where the bit lives | UTF-16 length |
|---|---|---|
| inline (≤ 23 bytes) | bit 0x40 of byte 23 (the byte length needs 5 bits, `0x80 \| len`) | computed from the ≤ 23 bytes (a SWAR count of non-continuation bytes, plus 4-byte leads) |
| static / borrowed | bit 63 of `w1` | the header (below) |
| heap | bit 63 of `w1` | the header (below) |

**3. A header before non-inline text.** Heap buffers become
`[crumbs: atomic ptr][meta: u64][count: atomic u64][bytes]`. The count stays at `ptr - 8`, so
retain and release are unchanged. `meta` holds:

- the UTF-16 length;
- an `ILL_FORMED` bit (the text holds a lone surrogate).

Every producer computes the header as it writes, so the length is always O(1):

- concatenation adds the two lengths, minus one where a pair is joined;
- the builder keeps a running count;
- decoding I/O counts while it validates UTF-8 (one pass, SIMD-friendly).

A **static** non-ASCII string (a literal) gets the same header in read-only data, with the
crumbs table already filled in by the compiler. A **borrowed** non-ASCII sub-range (a `split`
piece, or a JSON key pointing into the parsed text) has no header to point at, so it is copied
instead. That copy is inline when it is ≤ 23 bytes, which covers most keys. ASCII sub-ranges
still borrow.

**Breadcrumbs** translate a code-unit index into a byte offset for long non-ASCII text (Swift's
`String` uses the same technique for its UTF-16 view):

- a table of the byte offset of every 64th code unit, built on the first index operation on a
  string above 64 units;
- published with a compare-and-swap, because strings cross threads;
- freed with the buffer.

An index lookup is then one table load plus a forward scan of at most 63 units. The table costs
about 1/16 of the text's size (one `u32` per 64 units; `u64` above 4 GiB), and only for
non-ASCII strings that are indexed.

### How the operations compile

| Operation | ASCII (flag clear) | Non-ASCII |
|---|---|---|
| `length` | inline load, as today, plus one bit test | inline: SWAR count of ≤ 23 bytes; otherwise a load of `meta` |
| `charCodeAt(i)`, `s[i]`, `at`, `charAt` | inline bounds check plus byte load, as today | runtime call: breadcrumb, scan, decode; a 4-byte sequence gives its high or low surrogate |
| `slice`, `substring` | byte offsets, as today | translate both ends; an end between two halves of a pair re-encodes that half as a 3-byte lone surrogate (a copy) |
| `indexOf`, `includes`, `startsWith`, `endsWith` | byte search, as today | byte search on WTF-8 (self-synchronizing), then translate the found offset; a needle that starts with a lone low surrogate or ends with a lone high one takes a code-unit slow path, so it can match half of a pair |
| `<`, `sort()` | `memcmp`, as today | `memcmp` to the first differing byte, then compare the UTF-16 unit there. Byte order is code-point order, and it differs from code-unit order only between U+E000–U+FFFF and supplementary characters. |
| `==`, hash | unchanged | unchanged (canonical WTF-8) |
| `+`, template literals | unchanged, plus OR of the flags | as left, plus a join check when both parts are ill-formed |
| `for...of` | byte loop | `char` decode loop (WTF-8 lone surrogates decode as themselves) |

A loop `for (let i = 0; i < s.length; i++) s.charCodeAt(i)` over non-ASCII text costs an
amortized scan of about 32 units per step through the breadcrumbs. Two optimizations follow
later as separate changes, not as part of this design:

- strength reduction: the optimizer turns that loop into a cursor that advances one unit per
  step;
- a one-entry "last index → byte offset" cache per string.

### Boundaries

Text enters and leaves the program as UTF-8, as today. Only text with the `ILL_FORMED` bit needs
work at a boundary:

| Boundary | Behaviour |
|---|---|
| decoding (files, sockets, HTTP, stdin, databases) | unchanged: strict or lossy UTF-8 as today. Valid UTF-8 is valid WTF-8; decoding also computes the flags and the length. |
| writing (`console.log`, files, sockets, HTTP bodies, process arguments, environment) | well-formed text is written as is (zero-copy); a lone surrogate becomes U+FFFD, as Node's `Buffer.from(s)` and `TextEncoder` do |
| native packages (`&str`, `String`) | well-formed: borrowed as today; ill-formed: a converted copy (this replaces today's fatal "not UTF-8" error) |
| `JSON.parse` | `"\ud800"` escapes now keep the lone surrogate (JS behaviour) instead of U+FFFD |
| `JSON.stringify` | a lone surrogate is written as a `\udXXX` escape (ES2019 well-formed `JSON.stringify`) |
| string literals | the lexer accepts `"\uD83D"`; `"😀"` is stored as the joined 4-byte code point |
| regex | Rust's `regex::bytes` runs on the WTF-8 bytes; offsets are translated as for `indexOf` |

`Buffer.byteLength(s)` is always the stored byte length: a lone surrogate takes 3 bytes in WTF-8
and its U+FFFD replacement also takes 3.

**Regex semantics.** Velt patterns already match whole code points (`.` matches `😀`), like
JavaScript's `u` flag. Without `u`, JavaScript matches code units. This design keeps code-point
matching and documents it as a difference, alongside the existing ones (no lookaround and no
backreferences, because matching is linear-time). Only offsets change.

## Cost

- **ASCII text:**
  - `length` and `charCodeAt` gain one predictable bit test;
  - concatenation gains one OR;
  - nothing else changes.

  `bench/strings` (24 MB of ASCII scanned with `charCodeAt`) must stay within the 3% gate.
- **Heap strings** grow by 16 bytes of header. A heap string is already ≥ 24 bytes, and the
  allocator's size classes absorb part of it. `bench/hashmap` and `bench/sort` cover this.
- **Non-ASCII text:**
  - `length` is O(1);
  - an index or slice operation is O(1) amortized, with a bounded scan;
  - the breadcrumb table is built once per string, on first use.

  A new benchmark, `bench/strings_utf16` (the `strings` program with non-ASCII words in the
  lines), records the cost. The target is within 2× of the ASCII run, and within Node's time,
  which also falls back to two-byte strings here.
- **I/O** is unchanged for well-formed text. Ill-formed text (rare: only produced by slicing
  between the halves of a pair, by `fromCharCode` or by JSON escapes) is converted when written.

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
paragraph on byte lengths.

The `ts` blocks that print a length (types.md `label`) get non-ASCII examples, so the docs test
covers the new rule.

### Code in the repository that assumes byte offsets

- `std/csv.vlt` scans a `u8[]` and slices the string with those byte positions. It changes to
  scan with `charCodeAt` (the delimiter is ASCII, so the ASCII fast path applies).
- `std/regex.vlt` forwards the runtime's offsets. The runtime translates them.
- `std/cli.vlt`: help column widths now count code units, which aligns better. Short-flag
  clusters split per code unit, as in Node.
- `std/url`: decides per function. Byte work moves to `u8[]` (`utf8Encode`), and string
  positions stay positions.
- `std/uuid.vlt`, `std/path.vlt` and `std/datetime`: ASCII positions are unchanged.
- Runtime functions that take or return positions (`str_ops/*`, the regex and the JSON value
  length) convert at their entry and exit.
- Golden tests:
  - `lang/json_duplicate_keys` expects `lone.length` to be 4 (the lone surrogates are kept),
    not 8;
  - new tests cover `length`, `slice`, `indexOf`, `charCodeAt`, `s[i]`, `for...of`, ordering
    and regex offsets on non-ASCII and ill-formed text.
- `tests/difftest` drops its ASCII-only restriction for strings and regex subjects. Differential
  testing against Node is the acceptance test for this design.
- `velt_lsp` already converts compiler byte offsets to UTF-16 columns. Compiler source positions
  stay byte offsets, so it is unaffected.

### Contract changes

- `docs/internals/contracts/rt_abi.md` "Strings": the flag bits, the header and its invariants,
  canonical WTF-8, `str_cmp` in code-unit order.
- `rt_abi_async.md` §12.2: the "POC indexing model" paragraph is replaced by code units; the
  method table gains the new functions.
- `native_abi.md`: `str_new` validates and computes the header.
- `vir.rs`: unchanged (`STR_AGG` stays three words). Lowering of string literals emits the
  header for non-ASCII text, and the inline `length` and `charCodeAt` sequences gain the flag
  test.

## Alternatives considered

1. **Store UTF-16**, as Java, C# and JavaScript engines do. Indexing is O(1) with no tables, but
   every boundary converts: files, sockets, HTTP, JSON, the UTF-8-only regex engine and native
   packages. ASCII memory doubles. Rejected because the boundaries are where a server spends its
   time.
2. **Latin-1 or UTF-16 per string**, as V8 does. Pure ASCII still writes out without a copy, but
   Latin-1 text such as `"é"` converts at every boundary, the regex engine needs a converted copy
   of two-byte text, and every runtime function has two code paths. This is the closest
   competitor. WTF-8 plus breadcrumbs gets the same ASCII speed with one code path and zero-copy
   I/O for all well-formed text.
3. **Keep bytes and document the difference** (today). Rejected: it blocks sharing code with a
   TypeScript client (#326), and it is a silent divergence.
4. **Code-point indexing** (Python 3). It is neither JavaScript's rule nor cheaper.
5. **A per-module or per-project switch.** Rejected: it would give two meanings for `length`,
   against "one way of doing things".

## Implementation order

One PR each:

1. Representation: the flag bits, the heap and static headers, canonical WTF-8 in concatenation,
   the builder and decoding, and the contract updates. No visible change yet (lengths still
   report bytes). The benchmark gate runs here.
2. Semantics: code-unit `length`, positions and ordering in the runtime and lowering; the std
   migration (`csv`, `regex`, `url`, `cli`); golden and docs updates; `Buffer.byteLength`.
3. New APIs: `s[i]`, `for...of`, `at`, `charAt`, `codePointAt`, `String.fromCodePoint`,
   variadic `fromCharCode`, `isWellFormed`/`toWellFormed`, position arguments for
   `includes`/`startsWith`/`endsWith`, `TextEncoder`/`TextDecoder`.
4. Boundaries: lone surrogates in literals and in `JSON.parse`/`stringify`, U+FFFD on output and
   for native packages, regex offsets; difftest over non-ASCII strings.
5. Later, measured: strength reduction of index loops; the last-index cache.

## Decisions

1. `s[i]` out of range panics `index out of bounds`, like `xs[i]`; `s.at(i)` returns
   `string | null`.
2. `charCodeAt` out of range keeps returning -1 for now; `NaN` belongs to the number-semantics
   decision in #214, not to this design.
3. Regex keeps whole-code-point matching (like JavaScript's `u` flag), documented as a
   difference; only offsets change to code units.
4. There is no per-module or per-project switch: one meaning of `length`.
5. The type of `length` and of positions follows #214's decision for `Array.length`.
