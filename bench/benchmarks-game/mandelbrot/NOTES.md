# mandelbrot: port notes

Official description: https://benchmarksgame-team.pages.debian.net/benchmarksgame/description/mandelbrot.html
(N = 16000; QUICK_N = 200, `expected-quick.txt` = the official `mandelbrot-output.txt`, a binary
PBM).

## Sources and credits

| file | source |
|---|---|
| `rust/src/bin/mandelbrot.rs` | Rust #4 (Matt Watson, TeXitoi, Volodymyr M. Lisivka, Michael Cicotti, Ryohei Machida): fastest Rust, rayon, portable `F64x8` |
| `rust/src/bin/mandelbrot_st.rs` | the same program with rayon's global pool set to 1 thread (one added line in `main`): no fast single-threaded Rust is published |
| `main.go` | Go #4 (Martin Koistinen, Greg Buchholz, The Go Authors, Isaac Gouy, Bill Broadley, Sean Lake, Rodrigo Corsi, Anton Yuzhaninov): fastest Go, see "Deviations" |
| `main_st.go` | Go #8 (The Go Authors, after Greg Buchholz's C program): fastest single-threaded Go |
| `main.js` | Node.js #3, Andrey Filatkin (after Go #4): worker threads |
| `main_st.js` | Node.js #2 (Greg Buchholz's C #2, buffered by Isaac Gouy): fastest single-threaded Node |
| `main.velt` | idiomatic single-threaded Velt: one pixel at a time with early exit, bits packed into a `u8[]`, written with `stdout.write` |
| `main_mt.velt` | `main.velt` with blocks of 16 rows rendered by spawned tasks, collected with `Promise.all` |
| `main_opt.velt` | the Rust #4 shape on one thread: 8 pixels at a time in a Copy struct `F64x8` |

The official files are unchanged apart from a first `// Source:` line (`_st.rs` and `main.go`:
see below). `main.*` is the fastest published program, `main_st.*` the fastest
single-threaded one, and Rust `_st` the reference for single-threaded Velt.

## Deviations
- `main.go`: on arm64 the Go compiler fuses `inv*float64(xy) - 1.5` into a fused multiply-add,
  the initial coordinates change in the last bit and the unmodified Go #4 prints a different
  image (1 byte differs at N = 200 and at N = 16000; checked). The official site runs x86-64,
  where Go does not fuse. `main.go` wraps that product in `float64(...)`, the Go spec's way to
  forbid fusion; the inner loop is untouched (its fused ops do not change the output).

## Correctness
Every implementation prints the official image at N = 200 and the Rust image at N = 16000
(byte-for-byte), both Velt backends included. The Velt programs also handle sizes that are not a
multiple of 8 (N = 201: same bytes as a scratch Rust port and Go #8).

## Where the time goes

Timings are in [../RESULTS.md](../RESULTS.md).

- **`main.velt` and `main_mt.velt` use the scalar per-pixel loop, not a compiler gap.**
  `main.velt` iterates one pixel at a time and leaves the loop as soon as it escapes, the shape of
  the simple published programs (Go #8, Node #2). Rust #4 iterates 8 pixels in lock-step
  (4 × `fmul.2d` per multiply on NEON), tests for escape only every 4 iterations and only when
  the previous byte was all-escaped. A Rust program with exactly the `main.velt` loops runs at
  parity with `main.velt`.
- **`main_opt.velt`** writes the Rust #4 shape with a Copy struct of eight `f64` lanes
  (`F64x8 { l0 … l7 }` with `add` / `sub` / `mul` methods). LLVM vectorizes it like Rust's
  `[f64; 8]`: Copy structs of lanes are a usable SIMD idiom.
- **`main_mt.velt`** uses more CPU than `main.velt` (as Rust's multi-threaded program does over
  its single-threaded one): with all cores busy, the efficiency cores run the same rows more
  slowly.
- **Output.** Every variant writes the bitmap with `stdout.write`, straight from the array. Peak
  RSS is about twice the 32 MB bitmap for `main` and `main_opt` (65 MB, Rust 32 MB): the bitmap
  grows by `push`, so its capacity runs ahead of its length.
- **Cranelift** does not vectorize the 8 lanes: `main_opt.velt` takes about 2× the CPU of the
  LLVM build.
