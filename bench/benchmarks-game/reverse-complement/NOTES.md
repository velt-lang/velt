# reverse-complement: port notes

Rules: <https://benchmarksgame-team.pages.debian.net/benchmarksgame/description/revcomp.html>.
Stdin is the output of `fasta 25000000` (official size); quick stdin is `fasta 1000` = the
official 10 KB `revcomp-input.txt`, and `expected-quick.txt` is the official
`revcomp-output.txt`.

## Sources

| File | Program | Authors | Threads |
|---|---|---|---|
| `rust/src/bin/reverse-complement.rs` | [Rust #1](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/revcomp-rust-1.html) (fastest) | Ryohei Machida (inspired by C++ #2, Adam Kewley) | rayon |
| `rust/src/bin/reverse-complement_st.rs` | Rust #1 on a one-thread rayon pool | | 1 |
| `main.go` | [Go #6](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/revcomp-go-6.html) (fastest Go) | Dirk Moerenhout | all cores |
| `main_st.go` | [Go #2](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/revcomp-go-2.html) (fastest single-threaded Go) | K P anonymous, Andrew Martin | 1 |
| `main.js` | [Node #2](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/revcomp-node-2.html) (fastest Node with correct output) | Joe Farro, Jos Hirth, 10iii | 1 |
| `main.velt` | idiomatic Velt with Node Buffer idioms: all of stdin read with `readAllBytesSync`, headers found with `indexOf`, each sequence written backwards (complemented, 60-column lines) into one `Buffer.alloc` output, sent with one `stdout.write` | | 1 |
| `main_mt.velt` | stdin read in chunks (`openRead`), each sequence's output in 16384-line blocks, each block a spawned task over its own slice of the sequence | | all cores |

Deviations: Rust #1 uses the `memchr` crate, which the bench project does not depend on
(`rust/Cargo.toml`), so `memchr(b, s)` is a local `s.iter().position(|&x| x == b)`. On aarch64
its SSSE3 path is compiled out and the scalar `reverse_chunks` fallback is used (as on the official ARM runs).

Correctness: every implementation matches the official output at 1000 and the Rust output at
25,000,000 (Velt: both backends, both variants).

## Where the time goes

Timings are in [../RESULTS.md](../RESULTS.md). `main.velt` takes about 1.5× the CPU of Rust #1
on one thread.

- **Memory.** `main.velt` holds the whole input and the whole output (2 × 250 MB, about 580 MB
  peak); Rust #1 reverses in place in its read buffer (125 MB).
- **Bulk byte operations.** Like Rust #1 and Go #2, `main.velt` finds line ends with a byte
  search instead of scanning one byte at a time: `u8[]`'s `indexOf` is a 16-byte SIMD search in
  Velt (`std/lang/bytes/search.velt`).
- **`main_mt.velt` is slower than `main.velt`.** It parses its chunks byte by byte in `main` and
  each task takes a copy of its slice (`seq.slice(a, b)`).
- **Hot loops in `async` functions.** `velt_opt::frame_slots` keeps the scalar frame fields a
  poll function reads in a loop in locals, so a byte loop in `main_mt.velt`'s `async main`
  compiles like the same loop in a sync function.
