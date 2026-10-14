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
queries, paths and `data:` URLs, which `url_parse` has none of. 2,287M instructions against the
`url` crate's 716M on the same machine, 3.19: in text that is not ASCII every `charCodeAt` first
finds its position, and the parser reads the input that way.

Reading it:

- The four number rows call the same crates as the Rust column, so their gap is the cost of
  getting there: a call into the runtime per value and a string result, where Rust formats into
  a buffer on the stack.
- `sha2` uses the CPU's SHA-2 instructions on this machine (2 GB/s); `velt:crypto` is portable
  code. The gap to portable Rust code is smaller than the table says.
- `url_parse` has no IPv6 or non-ASCII host: `velt:url` has no IDNA yet, so those wait for it.
- Decompression (`flate2`, `brotli-decompressor`) has no row: Velt code can reach it only
  through `fetch`.
