# Design: building strings fast, in compiled code and in Velt

Status: proposed (#1677, part of the JSON engine, #1655). Nothing here is in `main` yet.

## Problem

Every string a program builds goes through the runtime's builder, `VeltStrBuf`
(`velt_rt/src/strbuf.rs`). That includes template literals, chains of `+`, `s += x` in a loop, and the
`JSON.stringify` and `console.log` glue. The builder has the layout of a `VeltStr`, so `finish`
is a move, which is good. But every append is a call into the runtime: `push_byte` for a `,`,
`push_bytes` for `{"name":`, `push_u64` for a number, `push_str` for a string. The fast path
inside each call is a handful of instructions: check the form, check room, check the count is 1,
copy, add to `w1`.

With the JSON writer written in Velt (#1668, a later pull request), a profile of
`bench/json/stringify` puts about two thirds of the time in those calls and the copies behind
them; the pull request that implements step 1 commits the profile's numbers with the script that
measures them.

Velt code has no better way to build strings. The Velt JSON writer (`std/json/writer.vlt`, in
the engine's `std/json/` modules, json-engine.md §0) passes the glue's builder
around as an opaque `string` and calls `velt_rt_strbuf_*` externs on it. That is a trap, as
#1668 found: an argument after it that is a call makes the compiler copy the "string" first, so
the append goes to the copy. It also needed a new primitive (`push_u8s`) to append an escaped
literal built in a `u8[]`.

## Proposal

Three steps, each useful alone.

### 1. Appends compiled inline (no new API)

When lowering pushes ASCII that it knows at compile time (one punctuation byte, the literal text
of a template or of JSON keys), it emits the builder's fast path inline and calls the runtime
only when it fails:

```text
w2 = buf.w2; tag = byte 23 of buf
if tag & 0xC0 == 0 && w2 != 0                      // a heap string, not a slice
   && (buf.w1 as u32) + n <= w2                    // room
   && atomic_load_acquire(buf.ptr - 8) == 1 {      // the only reference
  store the n bytes at buf.ptr + (buf.w1 as u32)
  buf.w1 += n * 0x1_0000_0001                      // n bytes, n units
} else {
  velt_rt_strbuf_push_bytes(buf, text, n)          // grows, or goes inline, as today
}
```

These are the checks of `VeltStr::push_ascii` (`str/push.rs`), on the layout rt_abi.md
documents, with the same acquire load as `heap::is_unique` (`velt_atomic_load_64_acq`, emitted
inline by both backends, #1619). A builder starts inline (≤ 23 bytes) and takes the slow path
until it moves to the heap, after which the appends stay inline.

- **Cost:** about ten instructions per append site, instead of a call.
- **Where:** in `JSON.stringify`'s glue, whose builder is private to it, so the fast path needs
  no count check. Template literals and `s += …` append onto variables that may share their
  buffer, so they would need the acquire-load count check at every site; in a first trial that
  cost far more IR than it saved there, so they keep the call (a later try: one module-level
  helper per byte count). The implementing pull request gives the measurements and the script
  behind them.

### 2. `StringBuilder`: the builder as a type (std, Velt)

```ts planned
const b = new StringBuilder();
b.append("key=");      // a string
b.appendCodePoint(0x3d); // a code point, as String.fromCodePoint
b.appendInt(42);       // decimal digits, written in Velt
const s = b.toString(); // the builder becomes the string: no copy (it has VeltStr's layout)
```

A std class whose one field is the builder value (the 24 bytes of a `VeltStrBuf`), declared as
a builder type the compiler knows. Its methods are Velt code over the same inline fast path as
step 1, so a call to `append` inlines to the step-1 sequence. The JSON writer then takes a
`StringBuilder` instead of an opaque `string`, and the generated glue passes its builder as one.
Integer formatting moves to Velt (`bench/stdport` `int_to_string` is 1.82× `itoa` today: a call
per value and a string result, where writing digits into the builder needs neither).

`appendCodePoint(c)` takes a code point, as `String.fromCodePoint` does (a non-code-point is
its `RangeError`). A lone surrogate is appended as one; a high surrogate followed by a low one
joins into the four-byte form, as `"\ud83d" + "\ude00" === "😀"` and the runtime's append do.

`toString()` hands the buffer over without a copy and leaves the builder empty, ready to build
another string (`StringBuilder` is a class, so a second reference to it cannot be caught at
compile time). Using it afterwards appends to a new, empty text.

### 3. Byte views of strings (reading)

`s.utf8()`: a read-only view of a string's bytes, where it keeps them. It supports `[i]`,
`length`, `subarray(a, b)`, `u8x16.load(view, i)`, and `b.appendBytes(view.subarray(a, b))` on a
`StringBuilder` (one copy, inline `memcpy`). A range can cut a UTF-8 sequence, or the four-byte
form of a surrogate pair, and a plain copy would break canonical WTF-8 and the UTF-16 count. So
`appendBytes` checks the two ends inline (one byte each): a range that cuts a surrogate pair's
four-byte form appends the cut half as a lone surrogate, as slicing the string between the two
UTF-16 units does (`"😀".slice(0, 1)`); a range that cuts any other sequence is a `RangeError`
panic (no position in a JavaScript string corresponds to it). The copy between stays one
`memcpy`, and the UTF-16 count is taken while copying (non-ASCII blocks only). Std parsers (`velt:json`, `velt:url`, `velt:csv`)
then need neither `__intrinsic_str_byte_at` nor `utf8ByteAt`'s check at each byte: the view is
bounds-checked once per loop where the compiler can prove the range, like an array.

## Not proposed

- **Making a `u8[]` a string without a copy.** A heap string keeps its reference count (and, for
  non-ASCII text, a header) in the bytes before its text; a `u8[]` is a plain buffer. A builder
  over a `u8[]` would need a final copy. Building in the string's own buffer (steps 1 and 2)
  needs none.

## Contracts

- Step 1 follows the `VeltStr` layout and `strbuf` semantics in rt_abi.md; neither changes. It
  makes generated code depend on the heap header (the count at `ptr - 8`), which compiled code
  reads already (drop and clone glue). rt_abi.md records the new reader.
- Step 2 adds a std type and, for the builder's field, a way to declare a builder value; the
  note for that step settles whether it needs a HIR change.

## Order

1. Inline appends (step 1). Measured on `bench/json/stringify` and the template literal
   benchmarks (`bench/typical` `strings`).
2. `StringBuilder`, and the JSON writer moved onto it; integer digits in Velt.
3. Byte views, and `velt:json/scan` moved onto them.
