# Building strings fast

Status: partly implemented. Issue: [#1677](https://github.com/velt-lang/velt/issues/1677).

## Summary

Every string a program builds goes through the runtime's builder, `VeltStrBuf`
(`crates/velt_rt/src/strbuf.rs`): template literals, chains of `+`, `s += x` in a loop, and the
`console.log` glue. The builder has the layout of a `VeltStr`, so finishing it is a move. Two
things make building fast beyond that: `StringBuilder`, a std class that is the builder as a type,
and inline appends: lowering compiles an append of literal ASCII text to the builder's fast path
instead of a runtime call. `JSON.stringify` writes through generated Velt code on a
`StringBuilder`, so its constant text (`{"id":`, `,"name":`) is appended inline.

## Motivation

Each append into the builder is a call into the runtime (`push_byte` for a `,`, `push_bytes` for
`{"name":`, `push_str` for a string), whose fast path is a handful of instructions: check the
form, check room, copy, add to `w1`. In a writer that appends many short literal pieces the calls
dominate. Velt code also needs a way to build strings without passing the runtime builder around
as an opaque `string`, which is a trap: an argument after it that is a call makes the compiler
copy the "string" first, so the append goes to the copy.

## Design

### `StringBuilder`

```ts
const b = new StringBuilder();
b.append("key=");
b.appendCodePoint(0x3d);
b.appendInt(42);
b.appendNumber(1.5);
const s = b.take();
console.log(s, b.length);
```

`StringBuilder` (`std/text/string_builder.velt`, a global) holds the builder in a private string
field; its methods call the runtime's builder functions on it.

- `append(s)` appends a string; `appendInt(n)` the decimal digits of an integer, written in Velt
  (`velt/lang/number/integer`); `appendNumber(x)` a number as `String(x)` writes it.
- `appendCodePoint(c)` appends a code point (or a UTF-16 code unit), as `String.fromCodePoint`
  does. A lone surrogate is appended as one, and a high surrogate followed by a low one joins into
  the four-byte form, as `"\ud83d" + "\ude00" === "😀"` does.
- `take()` hands the buffer over without a copy and leaves the builder empty, ready for another
  string. `StringBuilder` is a class, so a second reference to it cannot be caught at compile
  time; using it after `take()` appends to a new, empty text.
- `toString()`, which every conversion to a string calls (`${b}`, `String(b)`), keeps the text:
  it returns it shared, and the returned text never changes: an append through the runtime copies
  the shared buffer first, as for any string, and an inline append writes only after the text
  ("Inline appends"). A conversion that emptied its operand would make logging a builder lose its
  contents.
- `length` is the text's length in UTF-16 code units; `clear()` empties the builder.

### Inline appends

`b.append("literal")` with ASCII text of at most 32 bytes compiles to the builder's fast path
on the builder field (`crates/velt_vir/src/lower/strbuf.rs` `builder_append_literal`), with the
runtime call only on the slow path:

```text
w1 = buf.w1; w2 = buf.w2
end = (w1 & 0xffff_ffff) + n
if (w2 >> 62) == 0 && end <= w2 {         // a plain heap string (not inline, not a slice), room
  store the n bytes at buf.ptr + (w1 & 0xffff_ffff)
  buf.w1 = w1 + n * 0x1_0000_0001         // n bytes, n units
} else {
  velt_rt_strbuf_push_bytes(buf, text, n) // grows, or stays inline
}
```

These are the checks of `VeltStr::push_ascii` (`crates/velt_rt/src/str/push.rs`) on the layout
[rt_abi.md "Strings"](../contracts/rt_abi.md) documents, except the check that the buffer has no
other reference. A `StringBuilder`'s buffer may be shared (`toString()`), and writing after its
text is still safe: every other holder of the buffer is a string whose text ends at or before the
builder's, and that string appends in place only as the buffer's one holder (the runtime checks
the count), when the builder no longer has the buffer. So the bytes after the builder's text are
always the builder's. A builder made without a capacity larger than an inline string (23 bytes)
starts inline and takes the slow path until it moves to the heap; after that the appends stay
inline.

`strbufPushAsciiWords` (an intrinsic of `velt/lang/intrinsics`) appends up to 24 ASCII bytes held
in three words with the same fast path; std's integer and JSON number writers use it.

## Runtime cost

An inline append is about ten instructions instead of a call. On a loop of literal appends
(`append("{\"id\":")`, `appendInt`, three more literals) the inline path takes 0.745 G
instructions where calls take 1.95 G in a release build (-62%), and 1.13 G against 2.27 G in a
debug build.

Template literals and `s += …` keep the call: they append onto variables that may share their
buffer, so an inline path there would need the count check at every site, which costs more code
than it saves.

## Alternatives considered

**Making a `u8[]` a string without a copy.** A heap string keeps its reference count (and, for
non-ASCII text, a header) in the bytes before its text; a `u8[]` is a plain buffer. A builder over
a `u8[]` would need a final copy. Building in the string's own buffer needs none.

## Not yet implemented

**Byte views of strings.** `s.utf8()`: a read-only view of a string's bytes, where it keeps them,
with `[i]`, `length`, `subarray(a, b)`, `u8x16.load(view, i)`, and
`b.appendBytes(view.subarray(a, b))` on a `StringBuilder` (one copy, an inline `memcpy`). A range
can cut a UTF-8 sequence, or the four-byte form of a surrogate pair, and a plain copy would break
canonical WTF-8 and the UTF-16 count, so `appendBytes` checks the two ends: an offset inside a
four-byte form stands for the position between its two UTF-16 units, as slicing there does
(`"😀".slice(0, 1)`), and its half is appended as a lone surrogate; an end inside any other
sequence is a `RangeError` (no JavaScript string position corresponds to it). The UTF-16 count is
taken while copying. Std parsers (`velt/encoding/json`, `velt/net/url`, `velt/encoding/csv`) then
read bytes through a view that is bounds-checked once per loop where the compiler can prove the
range, like an array.
