# Design: building strings fast, in compiled code and in Velt

Status: proposed (#1677, part of the JSON engine, #1655). Step 1 is implemented for `JSON.stringify`'s glue (with the JSON engine's pull requests).

## Problem

Every string a program builds goes through the runtime's builder, `VeltStrBuf` (`velt_rt/src/
strbuf.rs`). That includes template literals, chains of `+`, `s += x` in a loop, and the
`JSON.stringify` and `console.log` glue. The builder has the layout of a `VeltStr`, so `finish`
is a move, which is good. But every append is a call into the runtime: `push_byte` for a `,`,
`push_bytes` for `{"name":`, `push_u64` for a number, `push_str` for a string. The fast path
inside each call is a handful of instructions: check the form, check room, check the count is 1,
copy, add to `w1`.

In `bench/json/stringify` with the Velt writer (#1668), those calls and the copies behind them
are about two thirds of the time:

| | self time |
|---|---:|
| `push_bytes` + `push_byte` + `push_u64` + `push_bool` + `str_append` | ~33% |
| `memmove` (growth, appended strings) | ~34% |
| integer formatting (`itoa`) | ~7% |

Velt code has no better way to build strings. `std/json/writer.vlt` passes the glue's builder
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
- **Measured:** in `JSON.stringify`'s glue, `bench/json/stringify` is 14–17% faster for +10% IR
  (+2.5% on `json_to_json`, +0.8% on the web server). That builder is private to the glue, so
  the fast path has no count check. In template literals and `s += …`, which append onto
  variables that may share their buffer and so need the acquire-load count check, it gained
  1–4% for 18–23% more IR in template-heavy code, so they keep the call. A later try: one
  module-level helper per byte count.

### 2. `StringBuilder`: the builder as a type (std, Velt)

```ts
const b = new StringBuilder();
b.append("key=");      // a string
b.appendCode(0x3d);    // one UTF-16 code unit or code point
b.appendInt(42);       // decimal digits, written in Velt
const s = b.toString(); // the builder becomes the string: no copy (it has VeltStr's layout)
```

A std class whose one field is the builder value (the 24 bytes of a `VeltStrBuf`), declared as
a builder type the compiler knows. Its methods are Velt code over the same inline fast path as
step 1, so a call to `append` inlines to the step-1 sequence. The JSON writer then takes a
`StringBuilder` instead of an opaque `string`, and the generated glue passes its builder as one.
Integer formatting moves to Velt (`bench/stdport` `int_to_string` is 1.82× `itoa` today: a call
per value and a string result, where writing digits into the builder needs neither).

`toString()` moves the value out. Using the builder after that is a compile error, as for any
moved value (memory.md).

### 3. Byte views of strings (reading)

`s.utf8()`: a read-only view of a string's bytes, where it keeps them. It supports `[i]`,
`length`, `subarray(a, b)`, `u8x16.load(view, i)`, and `b.appendBytes(view.subarray(a, b))` on a
`StringBuilder` (one copy, inline `memcpy`). Std parsers (`velt:json`, `velt:url`, `velt:csv`)
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
