# Standard-library components against the Rust crate for the same job

Each `<name>.velt` exercises one part of the standard library, and `rust/src/bin/<name>.rs` is the
same program on the crate a Rust program would use for it (`itoa`, `ryu`, `url`, `sha2`,
`base64`, `flate2`, `brotli`, …). Both print the same output; the harness fails otherwise.

```sh
bench/stdport/run.sh [runs] [only-benchmark]
```

It prints the best wall-clock time over the runs and, with valgrind on `PATH` (Linux), the
instructions each program executes. `bench/nightly.sh` counts the Velt programs every night.

## What this is for

Moving a part of the standard library from Rust into Velt has to make it no slower. A port is
measured here before and after: the Velt implementation replaces the runtime's when its row
executes no more instructions than before, and the aim is a `Velt / Rust` ratio below 1. A
runtime helper is replaced only where the Velt code is no slower on x86-64 Linux as well as
arm64, in instructions and wall time. A new port adds its row first, on the implementation it
is about to replace.

Each section below holds the latest measurement of its rows.

## Number text, URLs, base64

Apple M4, macOS, rustc 1.99, Apple clang 21; 2026-10-13, commit 8b06cc22. Best of 5 interleaved
runs, wall-clock milliseconds including process start.

| benchmark | Velt | Rust | Velt / Rust |
|---|---:|---:|---:|
| int_to_string | 70 | 39 | 1.82 |
| float_to_string | 88 | 30 | 2.96 |
| parse_float | 57 | 28 | 2.01 |
| to_fixed | 165 | 99 | 1.66 |
| url_parse | 161 | 64 | 2.51 |
| base64 | 149 | 41 | 3.64 |

The four number rows measured the runtime's formatting and parsing (the same crates as the Rust
column, reached through a call per value); number text is Velt code (`std/lang/number`), which
this table predates. `url_parse` has no IPv6 or non-ASCII host, because `velt/net/url` has no
IDNA.

`url_parse_wide` (URLs with a character that is not ASCII, and long queries, paths and `data:`
URLs): 2,324M instructions against the `url` crate's 730M, 3.18 (Apple M4, 2026-10-14, commit
0f229256e). In text that is not ASCII every `charCodeAt` first finds its position, and the
parser reads the input that way.

## Strings

`string_methods`, `string_methods_long` and `string_methods_wide` run the `String.prototype`
methods on short ASCII lines, long ASCII text, and text that is not ASCII (Latin with accents,
Cyrillic, CJK, emoji), each with literal needles and needles that vary. The Rust port runs
`str`'s methods with JS's positions in UTF-16 units (`rust/src/js_str.rs`). Apple M4, macOS,
instructions retired (`/usr/bin/time -l`, best of 5), 2026-11-11, commit 4be8d2c93: 2,365M,
3,398M and 2,109M against Rust's 2,183M, 2,398M and 2,127M (Node 24 on the same programs:
4,576M, 3,399M and 2,569M, start-up included). These predate the move of the methods from the
runtime into Velt (`std/lang/string/`, [#2198](https://github.com/velt-lang/velt/issues/2198)).

`split`, `trim`, `replace`, `repeat`, `padStart` / `padEnd` and the case mapping of ASCII text in
Velt against the runtime's Rust, the same compiler and runtime with only the std files swapped
(Linux x86_64, cachegrind, 2026-11-20): `string_methods` 1,321M instructions against 2,050M
(0.64), `string_methods_long` 1,918M against 2,829M (0.68), `string_methods_wide` 1,987M against
2,013M (0.99); `bench/strings_utf16`'s `csv`, which splits every line, 418M against 474M (0.88).

## Integers and random numbers

- `limb_mul` (64-bit limb arithmetic on `Math.umulh`, against `u128` in Rust): 976M instructions
  against Rust's 990M, 0.99 (Linux x86_64, cachegrind, 2026-11-16, commit f625a29d). The loop is
  unrolled twice, with one widening `mul` and an `adc` chain per limb and no bounds check.
- `bigint_mul`, `bigint_div` and `bigint_text`: `velt/math`'s `BigInt` (Velt code on 64-bit limbs)
  against `dashu-int`: products, quotients, and decimal and hex text at 3, 25, 248 and 991 limbs.
- `prng` (`Math.random()`'s wyrand generator, Velt code): 820M instructions against the Rust
  port's 1,300M built with `rustc -C opt-level=3`, 0.63 (release, Linux, cachegrind, 2026-11-08,
  commit 74125fdf6).

## SHA-2 and SHA-1

`velt/crypto` uses the CPU's SHA instructions through `velt/simd`, as `sha2` does. Linux arm64
(a VM on an Apple M4), best of 5, 2026-10-20, commit d5b1eb92f:

| benchmark | Velt ms | Rust ms | Velt M instr | Rust M instr | Velt / Rust (instr) |
|---|---:|---:|---:|---:|---:|
| sha256_large | 27 | 29 | 69 | 66 | 1.04 |
| sha256_small | 27 | 30 | 193 | 94 | 2.04 |

The hash loops are bound by the latency of the SHA instructions, not by how many instructions
run, so they are compared in cycles: the `cycles elapsed` that `/usr/bin/time -l` reports for
the process on macOS, which other processes' load does not change, as the difference between two
run lengths (natively on the M4, data in cache, median of 3, lower is faster):

| | Velt | Rust |
|---|---:|---:|
| SHA-256, cycles per byte | 1.37–1.39 | 1.41–1.42 (`sha2`) |
| SHA-1, cycles per byte | 1.38–1.39 | 1.38–1.39 (`sha1`) |
| `sha256_small`'s hash + `set`, cycles per call | 121–125 | 125–127 |
| HMAC-SHA1, 64-byte key, 250-byte message, cycles per call | 690–707 | 738–749 |
| HMAC-SHA256, same, cycles per call | 668–687 | 718–727 |

These cycle counts come from programs outside this harness (its Rust side has no `sha1` or `hmac`
crate). Rust's HMAC is `hmac` with the digest left on the stack. Velt's SHA-256 makes the next
sixteen message words after each four groups of rounds instead of between them, which keeps the schedule
off the rounds' dependency chain. `sha256_small` runs more instructions: the digest is a new
32-byte array per call where `sha2` returns it on the stack. `u8[].set` copies 16 to 64 bytes
without calling the runtime, as Rust's `copy_from_slice` is inline.

## Hex

`hex` is `velt/encoding`'s `hexEncode` and `hexDecode` against the `hex` crate. Instructions
executed (cachegrind, Linux arm64, `VELT_THREADS=1`, 2026-10-15, commit 4d83ca3c0): Velt 2,285M,
Rust 17,420M, 0.13. The Velt code works on SIMD vectors (https://veltlang.com/docs/std/simd/),
sixteen bytes per step when encoding and 32 digits when decoding; the crate goes digit by digit
through a table.

## Bytes

Instructions executed (cachegrind, `VELT_THREADS=1`) on an i9-12900HK (AVX2) with Ubuntu 24.04 in
WSL, from `bench/simd_bytes/cases.sh`; 2026-10-18, commit 71052675c:

| benchmark | Velt |
|---|---:|
| bytes_search | 957.3 M |
| utf8_decode | 7,768.4 M |
| utf8_valid | 3,850.8 M |

`indexOf` and `includes` (`bytes_search`) are Velt code on `u8x16` (`std/lang/bytes/search.velt`):
the first vector is tested on its own (most searches end within a few bytes), then four vectors
per step, and the last bytes are covered by one vector that ends at the array's end instead of a
byte loop; searches over 208 bytes run the same loop in a function of its own, so callers stay
small. `bench/simd_bytes/cases.sh` measures short and long searches. `utf8_decode` (Cyrillic
text with single spaces, and with ", ") is the row for text whose ASCII comes in short runs:
skipping ASCII 16 and 64 bytes at a time took 2.4 times fewer instructions on ASCII text but 4
to 16% more on text with short ASCII runs.

## gzip

`gunzip` is `velt/compress`'s `gunzip` against `flate2` (its default `miniz_oxide` backend):
the 1 MB JSON of `gen_inputs.rs`, gzip-compressed at the default level, decoded 50 times.
Instructions executed (cachegrind, x86-64 Linux, 4 cores), and best wall-clock of 5
interleaved runs; 2026-10-29, commit fbb930d1c:

| benchmark | Velt | Rust | Velt / Rust | Velt ms | Rust ms |
|---|---:|---:|---:|---:|---:|
| gunzip | 408 M | 628 M | 0.65 | 49 | 63 |

The CRC-32 the trailer is checked against folds 64 bytes per step with carry-less
multiplication (`u64x2.clmulLow` & co., PCLMULQDQ on x86-64, PMULL on arm64), as `crc32fast`
does: 24 M of the 408 M, where the slicing-by-8 tables it falls back to without the instruction
took 180 M (3.4 instructions per byte). The decoder refills its 64-bit bit buffer with one 8-byte
load per symbol, decodes a second literal without a refill, and copies matches 16 bytes at a
time; its output is allocated without zeroing (`Buffer.allocUnsafe`).

## gzip compression

`gzip_fast` and `gzip_default` are `velt/compress`'s `gzip` at levels 1 and 6 against `flate2`
(its default `miniz_oxide` backend) at the same levels: the 1 MB JSON of `gen_inputs.rs`. A new
component, not a port. Instructions per compression (cachegrind, arm64 Linux, 2026-11-08,
commit 34fa799c9): Velt is (3 runs − 1 run) / 2 of one program that links both
encoders, so start-up and the file read drop out; Rust is the port's total for 20 compressions,
less its one decode, ÷ 20. The size of one stream against stock zlib 1.3.2 and Node's bundled
zlib. These are arm64 counts: on x86-64, measured the same way for both sides, `gzip_fast` is 1.04
of Rust (19.2 M against 18.4 M).

| benchmark | Velt | Rust | Velt / Rust | Velt bytes | flate2 bytes | stock zlib bytes | Node bytes |
|---|---:|---:|---:|---:|---:|---:|---:|
| gzip_fast | 14.6 M | 15.3 M | 0.95 | 140,903 | 134,091 | 148,818 | 141,166 |
| gzip_default | 74.3 M | 107 M | 0.69 | 127,698 | 126,411 | 127,698 | 127,619 |

Levels 2 to 9 follow zlib (hash chains, lazy matching from level 4, zlib's parameters per level, a
candidate tried only when the two bytes at the end of the best match so far agree), so their sizes
track stock zlib's: at level 9 both give 126,039 bytes. Level 1 has no chains: one table of
six-byte prefixes, then one of three-byte prefixes on a miss, whose entries are positions modulo
2^16, so a window slide doesn't touch them. Over the interop corpus and a set of real files, level
1 comes to 1.004 of Node's size. Level 2 costs more than level 1 (23.5 M) and is larger on
this JSON (145,470 bytes), though smaller over the real files (5.11 MB against 5.34 MB).

## Brotli compression

`brotli_fast` and `brotli_default` are `velt/compress`'s `brotliCompress` at qualities 1 and 4
(`"fastest"` and `"optimal"`) against the `brotli` crate at the same qualities (window 2^22): the
1 MB JSON of `gen_inputs.rs`. A new component, not a port. Instructions per compression, measured
as for gzip above, and the size of one stream against Node's encoder (the reference encoder, as the
crate is); 2026-11-08, commit 34fa799c9.

| benchmark | Velt | Rust | Velt / Rust | Velt bytes | Node bytes |
|---|---:|---:|---:|---:|---:|
| brotli_fast | 45.3 M | 59.3 M | 0.76 | 76,379 | 96,467 |
| brotli_default | 60.9 M | 61.7 M | 0.99 | 47,575 | 47,346 |

The stream rows are what `compress()` in packages/web does for each response: streams from the
same JSON at quality 4, one encoder each (`BrotliEncoding`'s `encoder()` against the crate's
`CompressorWriter`), written once and finished. `brotli_stream_small` is 1,000 streams of 1 KB and
measures setting an encoder up; `brotli_stream_20k` is 300 of 20 KB and `brotli_stream_300k` 30 of
300 KB. Instructions of the whole program (cachegrind, arm64 Linux, `VELT_THREADS=1`) and the best
of 7 wall-clock runs:

| benchmark | Velt | Rust | Velt / Rust | Velt ms | Rust ms |
|---|---:|---:|---:|---:|---:|
| brotli_stream_small | 258 M | 248 M | 1.04 | 13 | 16 |
| brotli_stream_20k | 650 M | 504 M | 1.29 | 32 | 23 |
| brotli_stream_300k | 726 M | 628 M | 1.16 | 37 | 29 |

A one-shot input gets a zeroed table for its length. A stream has no size hint, so it gets the full
table (4 MiB at quality 4) without zeroing it, and a bitmap of the buckets it used, 32 KiB, which is
all it zeroes: a bucket is set when first used. Streams of 20 KB and more are still behind the
crate, though the one-shot rows are not.

From quality 4 the encoder splits literals into block types, each with its own prefix code, with
the reference's greedy splitter; it has no context modeling or static dictionary. Over the
interop corpus (tests/std/compress/interop) and a set of real files, its sizes against Node's at
qualities 0, 1, 4, 6, 9 and 11 are 0.92, 0.94, 1.01, 1.04, 1.04 and 1.15; quality 11 searches as 9
does.

## Brotli

`brotli_decode` is `velt/compress`'s `brotliDecompress` against `brotli-decompressor`
(read to the end through its `Decompressor` with a 64 KiB buffer): the 1 MB JSON of
`gen_inputs.rs` at quality 5 and 11
(window 2^22), 50 times each. Its inputs are written by `gen_inputs` with the `brotli` encoder
crate. Instructions executed (cachegrind, `VELT_THREADS=1`, x86-64 Linux, 4 cores under load,
2026-10-29, commit fbb930d1c), and per quality (100 decodes of one input):

| benchmark | Velt | Rust | Velt / Rust |
|---|---:|---:|---:|
| brotli_decode | 1,016 M | 1,400 M | 0.73 |
| quality 5 only | 954 M | 1,379 M | 0.69 |
| quality 11 only | 1,078 M | 1,424 M | 0.76 |

Best of 9 interleaved wall-clock runs on the same (busy) machine: 80 ms against 205 ms; most of
the Rust program's extra time is the kernel's (page faults of `read_to_end`'s growing `Vec`).
A 2.3 KB JSON document (213 bytes at quality 11) decoded 20,000 times: 95 K instructions per
decode against 171 K, mostly building the prefix code tables of the header.

The decoder keeps the bit reader, output position and command state in locals of one loop and
decodes prefix codes with one table lookup (two for codes over 8 bits) as the reference decoder
does; the input sits in a buffer of its own followed by zeros, so the bit reader loads 8 bytes
at a time with no check per byte and finds out afterwards whether a step read past the input.

## pow

`pow` is `**` on floats (`Math.pow`), Velt code in `std/lang/math/pow.velt`, against the `libm`
crate's fdlibm `pow`. Instructions executed (cachegrind, i9-12900HK, Linux x86-64 (Ubuntu 24.04
under WSL), 2 runs each, the same); 2026-11-18, commit c7b8c6bc2:

| benchmark | Velt | Velt without FMA | Rust | Velt / Rust |
|---|---:|---:|---:|---:|
| pow | 288.3 M | 543.8 M | 575.0 M | 0.50 |

The default x86-64 level has no FMA, so `pow` runs a copy compiled for it where the CPU has it
(every x86-64 CPU since 2013). Without (the second column, `VELT_SIMD_DISABLE=clmul`), where each
of its twenty `fma`s would be a call of C's `fma` (a software routine on such a CPU),
`std/lang/math/pow_fast.velt` computes the same bits from unfused steps whose roundings it
checks. On arm64, and with `--cpu x86-64-v3`, every `fma` is one instruction. On wasm32-wasip1,
which has no `fma` instruction (wasmtime 50), the program takes 525.9 M instructions
(precompiled).

## Integer powers

`pow_int` is `**` on `i64` (20 million pairs, exponents 0 to 63), Velt code in
`std/lang/hooks/pow_int.velt`, against the runtime's `velt_rt_pow_i64` it replaced (the Rust
column, called through a function that is not inlined, as compiled code called it). Instructions
executed (cachegrind, x86-64 Linux, release), 2026-11-21:

| benchmark | Velt | runtime call before | Rust | Velt / Rust |
|---|---:|---:|---:|---:|
| pow_int | 1,164.8 M | 1,463.6 M | 1,522.9 M | 0.76 |

Inlined at the call, the power squares before it multiplies, so its loop has no check of its own
for whether another square is due. Best of 7 interleaved runs: 54 ms, against 82 ms before and
97 ms for the Rust. A debug build (warm cache) takes 1,645.7 M against 1,843.8 M before.

## Map lookups by UTF-8 bytes

`map_get_utf8`: 4M `Map<string, i64>.getUtf8` lookups of keys given as bytes (short, long, not
ASCII; three in four present). The key's hash and comparison are Velt code
(`std/lang/map/utf8_key.velt`); they were the runtime's `velt_rt_bytes_hash` and
`velt_rt_str_eq_bytes`. x86-64 Linux, release, the same compiler and runtime with only std
swapped, cachegrind, 2026-11-20: 1,285M instructions before, 1,283M after (−0.2%); wall clock,
best of 7 interleaved, 103.7 → 86.5 ms. The Rust port (std's `HashMap` with the runtime's hash)
executes 521M.

## Symbols

`symbol_new`: 400k `Symbol(description)` at run time (some without one), then each one's
`description` and `toString()` twice. Symbols are Velt code (`std/lang/symbol/record.velt`,
records carved from per-thread chunks); they were the runtime's `velt_rt_symbol_new`,
`velt_rt_symbol_described` and `velt_rt_symbol_description`. x86-64 Linux, release, the same
compiler and runtime with only std swapped, cachegrind, 2026-11-20: 278M instructions before,
254M after (−8.7%); wall clock, best of 7 interleaved, 28.1 → 24.6 ms. The Rust port (a leaked
record sharing an `Rc<str>`, `format!` for the text) executes 984M.
