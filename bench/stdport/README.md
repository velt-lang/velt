# Standard-library components against the Rust crate for the same job

Each `<name>.vlt` exercises one part of the standard library, and `rust/src/bin/<name>.rs` is the
same program on the crate the runtime uses for it today (`itoa`, `ryu`, `url`, `dashu-int`), or,
where the standard library is already Velt code (`velt/crypto`, `velt/encoding`), on the crate a
Rust program would use (`sha2`, `base64`). Both print the same output; the harness fails
otherwise.

```sh
bench/stdport/run.sh [runs] [only-benchmark]
```

It prints the best wall-clock time over the runs and, with valgrind on `PATH` (Linux), the
instructions each program executes. `bench/nightly.sh` counts the Velt programs every night.

## What this is for

Moving a part of the standard library from Rust into Velt has to make it no slower. A port is
measured here before and after: the Velt implementation replaces the runtime's when its row
executes no more instructions than before, and the aim is a `Velt / Rust` ratio below 1.

A new port adds its row first, on the implementation it is about to replace.

## Baseline

Apple M4, macOS, `main` at 8b06cc22, rustc 1.99, Apple clang 21. Best of 5 interleaved runs,
wall-clock milliseconds including process start. No instruction counts: valgrind doesn't run on
this machine.

| benchmark | implementation today | Velt | Rust | Velt / Rust |
|---|---|---:|---:|---:|
| int_to_string | runtime, `itoa` | 70 | 39 | 1.82 |
| float_to_string | runtime, `ryu` | 88 | 30 | 2.96 |
| parse_float | runtime, Rust's `f64` parser | 57 | 28 | 2.01 |
| to_fixed | runtime | 165 | 99 | 1.66 |
| url_parse | Velt (`velt/net/url`) | 161 | 64 | 2.51 |
| bigint | runtime, `dashu-int` | 169 | 169 | 1.00 |
| sha256_large | Velt (`velt/crypto`) | 118 | 19 | 6.09 |
| sha256_small | Velt (`velt/crypto`) | 125 | 20 | 6.28 |
| base64 | Velt (`velt/encoding`) | 149 | 41 | 3.64 |

`limb_mul` was added after this table (64-bit limb arithmetic on `Math.umulh`, against `u128`
in Rust): 1,194M instructions against 888M on the same machine, 1.34.

`url_parse_wide` was added after it too: URLs with a character that is not ASCII, and long
queries, paths and `data:` URLs, which `url_parse` has none of. 2,324M instructions against the
`url` crate's 730M on the same machine, 3.18: in text that is not ASCII every `charCodeAt` first
finds its position, and the parser reads the input that way.

Reading it:

- The four number rows call the same crates as the Rust column, so their gap is the cost of
  getting there: a call into the runtime per value and a string result, where Rust formats into
  a buffer on the stack.
- `sha2` uses the CPU's SHA-2 instructions on this machine (2 GB/s); `velt/crypto` was portable
  code then. It now uses them too (#1330, `velt/simd`). Linux arm64 (OrbStack on an M4), best of
  5, ms and millions of instructions, Velt before → after, against Rust:

  | benchmark | Velt ms | Rust ms | Velt M instr | Rust M instr | Velt / Rust (instr) |
  |---|---:|---:|---:|---:|---:|
  | sha256_large | 115 → 27 | 29 | 2,380 → 69 | 66 | 36.0 → 1.04 |
  | sha256_small | 117 → 27 | 30 | 2,719 → 193 | 94 | 28.8 → 2.04 |

  (The machine was heavily loaded for the times.) The hash loops are bound by the latency of
  the SHA instructions, not by how many instructions run, so they are compared in cycles: the
  `cycles elapsed` that `/usr/bin/time -l` reports for the process on macOS, which other
  processes' load does not change, as the difference between two run lengths (natively on the
  M4, data in cache, median of 3, lower is faster):

  | | Velt | Rust |
  |---|---:|---:|
  | SHA-256, cycles per byte | 1.37–1.39 | 1.41–1.42 (`sha2`) |
  | SHA-1, cycles per byte | 1.38–1.39 | 1.38–1.39 (`sha1`) |
  | `sha256_small`'s hash + `set`, cycles per call | 121–125 | 125–127 |
  | HMAC-SHA1, 64-byte key, 250-byte message, cycles per call | 690–707 | 738–749 |
  | HMAC-SHA256, same, cycles per call | 668–687 | 718–727 |

  Rust's HMAC is `hmac` with the digest left on the stack. SHA-1 is at the latency of SHA1C
  for both. Velt's SHA-256 makes the next sixteen message words after each four groups of
  rounds instead of between them, which keeps the schedule off the rounds' dependency chain.
  `sha256_small` runs more instructions: the digest is a new 32-byte array per call where `sha2`
  returns it on the stack, and padding is built in registers. `u8[].set` copies 16 to 64 bytes
  without calling the runtime, as Rust's `copy_from_slice` is inline; the Velt program copies
  the digest into the message with `set` as the Rust port does with `copy_from_slice` (it used
  to copy it byte by byte).
- `url_parse` has no IPv6 or non-ASCII host: `velt/net/url` has no IDNA yet, so those wait for it.
- Decompression (`flate2`, `brotli-decompressor`) had no row then: Velt code could reach it
  only through `fetch`. See [gzip](#gzip) for `gunzip` and [Brotli](#brotli) for
  `brotli_decode`.

## Hex

`hex` is `velt/encoding`'s `hexEncode` and `hexDecode` against the `hex` crate. Instructions
executed (cachegrind, Linux arm64, `VELT_THREADS=1`):

| benchmark | Velt before | Velt | Rust | Velt / Rust |
|---|---:|---:|---:|---:|
| hex | 9,179 M | 2,285 M | 17,420 M | 0.13 |

The Velt code works on SIMD vectors (docs/std/simd.md), sixteen bytes per step when encoding
and 32 digits when decoding; the crate goes digit by digit through a table.

## Ported

A runtime helper is replaced only where the Velt code is no slower on x86-64 Linux as well as
arm64, in instructions and wall time (#1055). Instructions executed (cachegrind,
`VELT_THREADS=1`) on an i9-12900HK (AVX2) with Ubuntu 24.04 in WSL, merge base against this
code, from `bench/simd_bytes/cases.sh`:

| benchmark | was | x86-64: before | now |
|---|---|---:|---:|
| bytes_search | `memchr` | 1,080.4 M | 957.3 M |
| utf8_decode | Velt byte loop | 7,768.4 M | 7,768.4 M |
| utf8_valid | `str::from_utf8` | 3,850.8 M | 3,850.8 M |

`indexOf` and `includes` (`bytes_search`) are Velt code on `u8x16` when at most 208 bytes are
left from the start position: the first vector is tested on its own (most searches end within a
few bytes), then four vectors per step, and the last bytes are covered by one vector that ends
at the array's end instead of a byte loop. Longer searches still go to the runtime's `memchr`,
which takes 32 bytes per vector with AVX2 and is faster there (the Velt vectors executed 2.4 to 2.65 times as
many instructions on 4 KiB and 1 MiB). `bench/simd_bytes/cases.sh` measures both sides of that
length.

`utf8_decode` (Cyrillic text with single spaces, and with ", ") is the row for text whose ASCII
comes in short runs. `utf8DecodeLossy` keeps its byte loop: skipping ASCII 16 and 64 bytes at
a time took 2.4 times fewer instructions on ASCII text but 4 to 16 % more on text with short
ASCII runs, also with runs of up to four bytes stepped over one at a time. `xorMask` and `utf8Valid` keep
their runtime helpers.

## gzip

`gunzip` is `velt/compress`'s `gunzip` against `flate2` (its default `miniz_oxide`
backend, the crate the runtime's `fetch` decoded gzip and deflate bodies with): the 1 MB JSON of
`gen_inputs.rs`, gzip-compressed at the default level, decoded 50 times. Instructions executed
(cachegrind, x86-64 Linux, 4 cores), and best wall-clock of 5 interleaved runs:

| benchmark | Velt | Rust | Velt / Rust | Velt ms | Rust ms |
|---|---:|---:|---:|---:|---:|
| gunzip | 408 M | 628 M | 0.65 | 49 | 63 |

The CRC-32 the trailer is checked against folds 64 bytes per step with carry-less
multiplication (`u64x2.clmulLow` & co., PCLMULQDQ on x86-64, PMULL on arm64), as `crc32fast`
does: 24 M of the 408 M, where the slicing-by-8 tables it falls back to without the instruction
took 180 M (3.4 instructions per byte). The decoder refills its 64-bit bit buffer with one 8-byte
load per symbol, decodes a second literal without a refill, and copies matches 16 bytes at a
time; its output is allocated without zeroing (`Buffer.allocUnsafe`), which was 21 % of the
instructions before.

## Brotli

`brotli_decode` is `velt/compress`'s `brotliDecompress` against `brotli-decompressor`
(the crate the runtime's `fetch` decoded `br` bodies with, read to the end through its
`Decompressor` with a 64 KiB buffer): the 1 MB JSON of `gen_inputs.rs` at quality 5 and 11
(window 2^22), 50 times each. Its inputs are written by `gen_inputs` with the `brotli` encoder
crate. Instructions executed (cachegrind, `VELT_THREADS=1`, x86-64 Linux, 4 cores under load),
and per quality (100 decodes of one input):

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
