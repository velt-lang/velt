# JavaScript string semantics

Status: partly implemented. Issue: [#377](https://github.com/velt-lang/velt/issues/377).

## Summary

A Velt `string` is a sequence of UTF-16 code units, exactly as in JavaScript: `length`, every
position, indexing and ordering count or compare code units, and lone surrogates are ordinary
contents. The bytes stay UTF-8 (canonical WTF-8), so I/O stays zero-copy, and every string value
carries its UTF-16 length, so `length` is a load and ASCII text keeps its speed.

## Motivation

UTF-8 byte counts and UTF-16 unit counts agree only for ASCII. A runtime that counts bytes gives
different answers from TypeScript, silently:

```ts
console.log("héllo".length); // 5
console.log("héllo".slice(0, 2)); // "hé"
console.log("€".charCodeAt(0)); // 8364
console.log("😀".length); // 2
console.log("～" < "\u{1F600}"); // false
```

A validator shared between a browser and a Velt server (`name.length <= 3`) must accept and
reject the same names on both sides. The design gives exactly JavaScript's semantics under three
constraints: ASCII text keeps its speed (every benchmark within 3%); I/O stays zero-copy (files,
sockets, HTTP, JSON, native packages and the regex engine speak UTF-8); non-ASCII text pays a
bounded, predictable cost, never O(n) per sequential index.

## Design

| API | Meaning (as in JavaScript) |
|---|---|
| `s.length` | number of UTF-16 code units (`"😀".length` is 2) |
| `slice`, `substring`, `indexOf`, `lastIndexOf`, `includes`, `startsWith`, `endsWith`, `padStart`, `padEnd`, `at`, `charAt`, `charCodeAt`, `codePointAt` | positions and lengths in code units |
| `s[i]` | `s.charAt(i)`: the code unit at `i` as a one-unit string, `""` out of range |
| `s.at(i)` | the same, or `undefined` out of range |
| `for (const c of s)`, `[...s]` | code points (a surrogate pair is one element), as JavaScript's string iterator |
| `split("")`, `replaceAll("", x)` | code units (a pair splits into two lone surrogates) |
| `String.fromCharCode(...units)`, `String.fromCodePoint(...cps)` | as JavaScript; `String.fromCharCode(0xD83D, 0xDE00) === "😀"` |
| `<`, `<=`, `>`, `>=`, `sort()` without a comparator | code-unit order |
| `==`, `Map`/`Set` keys | same code units |
| regex `index` and positions | code units |
| `localeCompare` | CLDR root collation |

Lone surrogates are ordinary contents: `"😀".slice(0, 1)` is the one-unit string `"\uD83D"`,
and joining the two halves gives `"😀"` again. Searches can match half of a pair:
`"😀".indexOf("\uDE00")` is 1, `"a😀b".split("\uDE00")` is `["a\ud83d", "b"]`, and
`"😀".replaceAll("", "-")` is `"-\ud83d-\ude00-"`.

`charCodeAt` out of range returns -1, where JavaScript returns `NaN`.

Byte counts and byte views keep their JavaScript names. `Buffer.byteLength(s)`, the UTF-8 length,
is O(1); with an encoding argument it follows Node (`utf16le` is `2 × length`). `TextEncoder`
writes UTF-8, a lone surrogate as U+FFFD; `encodeInto` never splits a code point and its `read`
counts code units. `TextDecoder` decodes UTF-8: lossy by default, `fatal` throws, one leading
byte order mark is dropped unless `ignoreBOM`. There is no Velt-only byte-offset string API.

### Representation: WTF-8 with the UTF-16 length in the value

The value layout is in [rt_abi.md "Strings"](../contracts/rt_abi.md); this section gives the
reasons.

**Canonical WTF-8.** UTF-8 cannot encode lone surrogates. WTF-8 (the encoding Rust uses for
`OsString` on Windows) is UTF-8 that also allows a surrogate code point as a 3-byte sequence
(`ED A0..BF xx`), so it holds every UTF-16 sequence. In canonical form a surrogate *pair* is
always stored as its 4-byte code point, never as two 3-byte halves. Because of that rule byte
equality is code-unit equality, and hashing the bytes stays correct.

**The unit count is in the value.** For heap and static strings, `w1` packs `units:32 | bytes:32`.
Every non-ASCII character has more bytes than units, so **a string is ASCII exactly when
`units == bytes`**, with no flag bit.

| Form | Byte length | Unit length | ASCII test |
|---|---|---|---|
| static / borrowed, heap | low half of `w1` | high half of `w1` | `units == bytes` |
| inline, ASCII (≤ 23 bytes) | byte 23 = `0x80 \| len` | the byte length | bit 0x40 of byte 23 clear |
| inline, non-ASCII (≤ 22 bytes) | byte 23 = `0xC0 \| len` (`0x20`: may hold lone surrogates) | byte 22 | bit 0x40 of byte 23 set |

- The `length` read branches on the form: an inline string selects byte 22 or byte 23's length,
  any other reads `w1 >> 32` (`crates/velt_vir/src/lower/strings.rs`). It is a branch rather
  than a chain of selects, which is slower in Cranelift on scan loops, and LLVM unswitches the
  loop per form.
- A string is shorter than 2 GiB (at most `i32::MAX` bytes); compiled code reads the byte count
  sign-extended, which keeps LLVM from vectorizing `charCodeAt` scan loops into slower code. Node
  caps strings at 2²⁹ − 24 units, at most about 1.6 GB of WTF-8, so no program that works on
  Node is limited by it. The producers that can reach the limit (file reads, HTTP bodies, child
  output, stdin, the builder) report "string too long", checked where a buffer's layout is
  computed.
- A **borrowed** non-ASCII view (a `split` piece of a literal, a JSON key pointing into the
  parsed text) keeps its unit count in `w1`, with no copy. A JSON key of more than 64 units that
  is not ASCII is copied instead, so a long non-ASCII view always points at a literal.

**A header only on non-ASCII heap buffers.** ASCII buffers are `[count][bytes]`. A non-ASCII
buffer is `[crumbs][lone][count][bytes]`; the count stays at `ptr - 8`, so retain is unchanged,
and release, growth and free take the header size from the value (`units != bytes`).

- `lone` counts the lone surrogates. It decides whether a seam can join and whether output needs
  conversion; a count survives a join where a boolean would not:
  `lone(a + b) = lone(a) + lone(b) − 2 · joined`. A static string has no room for the count, so
  a buffer that absorbs text from one marks its count unknown, and it is counted when needed.
- An ASCII buffer moves behind a header at the append that writes its first byte ≥ 0x80, since
  template lowering and `s += x` use the live builder value.

**One way in.** `VeltStr::push_wtf8` (`crates/velt_rt/src/str/push.rs`) is how bytes enter a
string: it updates the unit count, the lone count and the layout, and joins a trailing high
surrogate to a leading low one when both sides have lone surrogates. A join keeps the unit count
and shortens the bytes by 2. The producers that can glue halves together go through it or through
`velt_text::wtf8::push_joining`: concatenation and the builder (templates, `Array.join`,
`s += x`), `repeat`, `padStart`/`padEnd`, `replace`/`replaceAll` and regex `replace`, JSON escape
decoding, `String.fromCharCode`/`fromCodePoint`, and on the compiler side the lexer and constant
folding. The debug runtime recounts every produced string and checks its canonical form
(`crates/velt_rt/src/str/invariants.rs`).

**Breadcrumbs.** For random access into long non-ASCII text (Swift's `String` uses the same
technique for its UTF-16 view, `crates/velt_rt/src/str/crumbs.rs`): a table of the byte offset of
every 64th code unit, built on the first position translation in a heap string of more than 64
units, published with a compare-and-swap because strings cross threads, and freed with the
buffer. A lookup is one table load plus a forward scan of at most 63 units; the table costs about
1/16 of the text's size, and only for non-ASCII strings that are indexed.

**Sequential indexing.** Breadcrumbs alone would make `for (i < s.length) s.charCodeAt(i)` over
non-ASCII text many times slower than Node. The runtime therefore remembers each thread's last
two translations of long non-ASCII strings (`crates/velt_rt/src/str/recent.rs`), keyed by
address, `w1` and form, and a translation near a remembered one steps from it; a one-unit step
decodes one character. Keeping this with the thread rather than in the string avoids bouncing
the count's cache line between cores reading one string. Freeing or growing a heap buffer that
has breadcrumbs bumps a global epoch that forgets the heap entries, so a new string at the same
address is never taken for the old one. A long non-ASCII static string points at a literal, which
is never freed, so its entries never expire, and a forward step of any length in it is taken
unless the end is closer, so a forward loop with gaps (a regex `exec` loop) stays linear. Every
position-taking operation benefits (`slice(i, i + 1)` and `indexOf(x, pos)` loops too).

**Slices.** A heap `slice` that is at least a quarter of its buffer shares it
(`crates/velt_rt/src/str/slice.rs`), so `rest = rest.slice(n)` parsers are linear, as with V8's
sliced strings. Smaller pieces are copied (they would pin a buffer more than four times their
size). A slice has no breadcrumbs of its own: it translates positions by a scan from the closer
end, and the per-thread positions keep sequential loops over it linear.

### How the operations compile

| Operation | ASCII (`units == bytes`) | Non-ASCII |
|---|---|---|
| `length` | inline load | the same |
| `charCodeAt(i)`, `s[i]`, `at`, `charAt` | inline bounds check and byte load | position translation, then a decode; a 4-byte sequence gives its high or low surrogate |
| `slice`, `substring` | byte offsets | translate both ends; an end between two halves of a pair re-encodes that half as a lone surrogate (a copy) |
| `indexOf`, `includes`, `startsWith`, `split`, `replace`, … | byte search | byte search on WTF-8 (self-synchronizing), then translate the found offset; needles that start with a lone low surrogate or end with a lone high one, positions inside a pair and the empty needle take a code-unit path (`crates/velt_rt/src/str_ops/units.rs`) |
| `<`, `sort()` | `memcmp` | the ordering rule below |
| `==`, hash | bytes | bytes (canonical WTF-8) |
| `+`, template literals | unit counts add | plus a join check when both parts have lone surrogates |

**The ordering rule** (`crates/velt_rt/src/str/order.rs` `cmp_utf16`). Byte order is code-point
order, which differs from code-unit order between U+E000–U+FFFF and supplementary characters, and
between lone surrogates and supplementary characters (`"\uDC00" > "\u{10000}"` is `true` in
Node). With `memcmp` as the fast path:

1. find the first differing byte, then step back to the start of that code point (the prefix is
   shared, so it is the same position in both strings);
2. compare the first UTF-16 unit of each side's code point: `cp` below 0x10000, else
   `0xD800 + ((cp − 0x10000) >> 10)`;
3. if they are equal (two supplementary characters with the same high surrogate, or one of them
   a lone high surrogate), compare the pair's low surrogate with the other side's next first
   unit, or with the end of the string. In canonical form one extra comparison suffices.

The rule is checked against a `Vec<u16>` reference model of the string operations
(`crates/velt_rt/tests/abi/utf16_model.rs`); the VIR interpreter's string comparison uses the same
rule.

### Boundaries

Text enters and leaves the program as UTF-8. Only text with lone surrogates (`lone > 0`) needs
work at a boundary. One lossy conversion maps each lone surrogate to exactly **one** U+FFFD
(Rust's `from_utf8_lossy` would give three; `velt_text::wtf8::to_utf8_lossy`), so
`Buffer.byteLength(s)` is also the output length: a lone surrogate and U+FFFD both take 3 bytes.

| Boundary | Behavior |
|---|---|
| decoding (files, sockets, HTTP, stdin, databases) | strict or lossy UTF-8. Surrogate encodings in external input are invalid UTF-8: strict mode refuses them, lossy mode replaces them, and they are never joined (Node decodes `ED A0 BD ED B8 80` as six U+FFFD). Decoding also counts units. |
| OS input (arguments, environment, paths) | lossy, both ways, as in Node |
| writing (files, sockets, HTTP bodies, process arguments, environment) | well-formed text is written as is (zero-copy); a lone surrogate becomes U+FFFD |
| `console.log` | a top-level string prints its lone surrogates as U+FFFD; a string nested in an array or object is escaped as `inspect` does (`console.log(["\uD83D"])` prints `[ '\ud83d' ]`) |
| native packages | `str_of` returns a `Cow`: well-formed text is borrowed, ill-formed text is a converted copy; `str_new` scans and canonicalizes its input with the decoding rule above |
| `JSON.parse` | `"\ud800"` escapes keep the lone surrogate, and an escaped pair joins |
| `JSON.stringify` | a lone surrogate is written as a `\udXXX` escape (well-formed `JSON.stringify`) |
| `encodeURIComponent`, `decodeURIComponent` | a lone surrogate, or `%ED%A0%BD`, throws `URIError` |
| string literals | escaped pairs (`"😀"`, `"\u{D83D}\u{DE00}"`) are one character |

**Regex.** Matching runs on the WTF-8 bytes of the subject, where they are, and offsets are
translated to code units at entry and exit (`std/text/regex/offsets.velt`). With the `u` flag a
character is a code point. Without it a character is a code unit, and a pair is two: its high
surrogate at the first byte of its 4-byte sequence and its low surrogate at the second, so each
half is matched on its own, as in JavaScript, with no copy of the subject
([the regular expression engine](regex-engine.md)).

## Runtime cost

- **ASCII text:** `length` reads `w1 >> 32`; `charCodeAt` tests `units == bytes`; concatenation
  adds unit counts. ASCII strings allocate exactly as without unit counts; `s[i]`, `charAt` and
  `for...of` items are inline strings (no allocation).
- **Heap strings:** ASCII buffers have no header; non-ASCII buffers carry 16 more bytes, part of
  the string's own allocation.
- **Non-ASCII text:** `length` is a load. Sequential index loops step from the thread's last
  position. Random indexing of large non-ASCII strings is a breadcrumb lookup plus a scan of at
  most 63 units, several times slower than Node's UTF-16 indexing.
- **In-place append** (`s += x` on a uniquely owned string) stays O(1): units add, the layout
  moves once to a buffer with a header, and breadcrumbs are extended lazily.
- **I/O** is unchanged for well-formed text. Text with lone surrogates (rare: only produced by
  slicing between the halves of a pair, by `fromCharCode` or by JSON escapes) is converted when
  written.
- `bench/strings_utf16` measures a `charCodeAt` loop over non-ASCII text, a sort of non-ASCII
  keys, split and join of non-ASCII CSV, JSON with emoji and an append loop, each against Node.

## Compatibility

- Code that used `length` as a byte count (buffer sizes, a computed `Content-Length`, wire
  protocols) should use `Buffer.byteLength(s)`.
- `charCodeAt` out of range is -1, not `NaN`.
- A lone surrogate escape in a string literal (`"\uD800"` alone) is a compile error; a lone
  surrogate made at run time (`String.fromCharCode(0xD800)`, slicing, `JSON.parse`) is fine.
- Compiler source positions stay byte offsets; `velt_lsp` converts them to UTF-16 columns.

## Alternatives considered

**Store UTF-16**, as Java, C# and JavaScript engines do. Indexing is O(1) with no tables, but
every boundary converts: files, sockets, HTTP, JSON, the UTF-8 regex engine and native packages,
and ASCII memory doubles. The boundaries are where a server spends its time.

**Latin-1 or UTF-16 per string**, as V8 does. Pure ASCII still writes out without a copy, but
Latin-1 text such as `"é"` converts at every boundary, the regex engine needs a converted copy of
two-byte text, and every runtime function has two code paths. WTF-8 with per-thread positions and
breadcrumbs gets the same ASCII speed with one code path and zero-copy I/O for all well-formed
text.

**Keep UTF-8 byte semantics and document the difference.** It blocks sharing code with a
TypeScript client and is a silent divergence.

**Code-point indexing** (Python 3). It is neither JavaScript's rule nor cheaper.

**A per-module or per-project switch.** It would give `length` two meanings.

**A flag bit plus a UTF-16 length in a header.** The unit count in `w1` makes `length` a load for
every form, needs no header on ASCII buffers, and lets borrowed non-ASCII views keep their length
without a copy.

## Limitations

- Long non-ASCII literals and borrowed views have no breadcrumbs: a translation in one is a scan
  bounded by its size, or a step from the thread's last position.

## Not yet implemented

- **Strength reduction of index loops:** `for (i < s.length) … s.charCodeAt(i) / s[i]` becomes a
  walk that advances one unit per step.
- **`isWellFormed()` and `toWellFormed()`** (ES2024).
- **Lone surrogates in literals:** `"\uD83D"` as a string literal, which needs string literals in
  the AST and HIR (`Lit::Str`) to carry WTF-8 bytes instead of a Rust `String`.
- **Lone surrogates in `.` and negated classes** (`[^…]`, `\S`, `\W`, `\D`) of `u`-mode
  patterns.
