# velt:compress

Decompression, written in Velt: gzip (RFC 1952), zlib (RFC 1950) and raw DEFLATE (RFC 1951) in
`velt:compress/inflate`, Brotli (RFC 7932) in `velt:compress/brotli`. They are what
[`fetch`](fetch.md) decodes compressed bodies with. A stream that is not valid, or that ends
before it should, throws `CompressError` (from `velt:compress/error`, also exported by both
modules).

## Whole streams

`gunzip(bytes)`, `inflate(bytes)` (zlib), `inflateRaw(bytes)`, `inflateHttp(bytes)` (HTTP's
`content-encoding: deflate`: zlib, or raw DEFLATE when the data has no zlib header) and
`brotliDecompress(bytes)` take the whole compressed stream and return the bytes it decodes to.
`gunzip` decodes several gzip members in a row as one stream, as the `gzip` tool writes them,
and sizes its output from the trailer, so the output is written once. The gzip and zlib checks
(CRC-32, Adler-32) are verified. Empty input decodes to nothing for `gunzip`, `inflateHttp` and
`brotliDecompress`.

```ts
import { gunzip } from "velt:compress/inflate";

function main() {
  // "hi\n", gzip-compressed.
  const gz: u8[] = [31, 139, 8, 0, 0, 0, 0, 0, 0, 3, 203, 200, 228, 2, 0, 122, 122, 111, 237, 3, 0, 0, 0];
  console.log(gunzip(gz).utf8Slice()); // hi
}
```

## Streams that arrive in pieces

`new InflateDecoder(format)` (`"gzip"`, `"zlib"`, `"raw"` or `"deflate"`, which is
`inflateHttp`'s choice) and `new BrotliDecoder()` decode a stream that arrives in pieces:
`feed(bytes)` queues compressed bytes, `finish()` says no more follow, and `read(max)` returns
up to `max` decoded bytes, `[]` when it needs more input first or when the stream has ended
(`done` is then true). The output comes in pieces of at most `max` bytes however much the input
expands, so a small body that decodes to gigabytes can be processed a piece at a time. Only the
window a stream may still refer back to is kept (32 KiB for DEFLATE, up to 16 MiB for Brotli).

```ts
import { InflateDecoder } from "velt:compress/inflate";

function main() {
  const gz: u8[] = [31, 139, 8, 0, 0, 0, 0, 0, 0, 3, 203, 200, 228, 2, 0, 122, 122, 111, 237, 3, 0, 0, 0];
  const d = new InflateDecoder("gzip");
  for (const b of gz) {
    d.feed([b]);
  }
  d.finish();
  let text = "";
  while (!d.done) {
    text += d.read(2).utf8Slice();
  }
  console.log(JSON.stringify(text)); // "hi\n"
}
```

## Speed

Against the Rust crates the runtime used before ([bench/stdport](../../bench/stdport/README.md),
instructions on x86-64): gzip decodes in 0.65 of `flate2`'s instructions, Brotli in 0.73 of
`brotli-decompressor`'s. The gzip CRC-32 is folded with the CPU's carry-less multiplication
([velt:simd](simd.md#carry-less-multiplication)) where it has it, tables elsewhere.
