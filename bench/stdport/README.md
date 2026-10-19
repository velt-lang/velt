# Standard-library components against the Rust crate for the same job

Each `<name>.vlt` exercises one part of the standard library, and `rust/src/bin/<name>.rs` is the
same program on the crate the runtime uses for it today (`itoa`, `ryu`, `url`, `dashu-int`), or,
where the standard library is already Velt code (`velt:crypto`, `velt:encoding`), on the crate a
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
| url_parse | Velt (`velt:url`) | 161 | 64 | 2.51 |
| bigint | runtime, `dashu-int` | 169 | 169 | 1.00 |
| sha256_large | Velt (`velt:crypto`) | 118 | 19 | 6.09 |
| sha256_small | Velt (`velt:crypto`) | 125 | 20 | 6.28 |
| base64 | Velt (`velt:encoding`) | 149 | 41 | 3.64 |

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
- `sha2` uses the CPU's SHA-2 instructions on this machine (2 GB/s); `velt:crypto` was portable
  code then. It now uses them too (#1330, `velt:simd`). Linux arm64 (OrbStack on an M4), best of
  5, ms and millions of instructions, Velt before → after, against Rust:

  | benchmark | Velt ms | Rust ms | Velt M instr | Rust M instr | Velt / Rust (instr) |
  |---|---:|---:|---:|---:|---:|
  | sha256_large | 115 → 15 | 14 | 2,380 → 69 | 66 | 36.0 → 1.04 |
  | sha256_small | 117 → 16 | 16 | 2,719 → 178 | 94 | 28.8 → 1.89 |

  `sha256_large` runs the same block loop as `sha2`: 2,700 MB/s natively on the M4 against
  2,765 (SHA-1: 3,015 against 2,955). `sha256_small` (500,000 one-block hashes) takes the same
  time; its extra instructions are the digest, a new 32-byte array per call where `sha2` returns
  it on the stack, and the runtime call of `set`, which copies the digest into the message as
  the Rust port's `copy_from_slice` does (the Velt program used to copy it byte by byte). HMAC
  with a 64-byte key and a 250-byte message (callgrind, instructions per call, the digest
  returned as a `Vec` in Rust): HMAC-SHA1 1,368 against 1,466, HMAC-SHA256 1,345 against 1,417.
- `url_parse` has no IPv6 or non-ASCII host: `velt:url` has no IDNA yet, so those wait for it.
- Decompression (`flate2`, `brotli-decompressor`) has no row: Velt code can reach it only
  through `fetch`.

## Hex

`hex` is `velt:encoding`'s `hexEncode` and `hexDecode` against the `hex` crate. Instructions
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
