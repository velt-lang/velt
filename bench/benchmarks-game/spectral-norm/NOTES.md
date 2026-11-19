# spectral-norm: port notes

Official description: https://benchmarksgame-team.pages.debian.net/benchmarksgame/description/spectralnorm.html
(N = 5500; QUICK_N = 100, `expected-quick.txt` = the official `spectralnorm-output.txt`).

## Sources and credits

| file | source |
|---|---|
| `rust/src/bin/spectral-norm.rs` | Rust #5 (Rust Project Developers, Matt Brubeck, TeXitoi, Tung Duong, Cristi Cobzarenco, Andre Bogus): rayon, portable `F64x2` struct. Rust #6 (same authors, ranked equal) has an x86-only SSE path |
| `rust/src/bin/spectral-norm_st.rs` | the same program with rayon's global pool set to 1 thread (one added line in `main`): no single-threaded Rust program is published |
| `main.go` | Go #4, K P anonymous / Isaac Gouy: fastest Go, 4 goroutines (`nCPU = 4` is hard-coded) |
| `main_st.go` | Go #1, chaishushan: fastest single-threaded Go (tied with Go #8 officially) |
| `main.js` | Node.js #6, Ian Osgood / Roy Williams / Isaac Gouy / Andrey Filatkin: worker threads, one per CPU |
| `main_st.js` | Node.js #1, Ian Osgood / Isaac Gouy: single-threaded Node (officially 0.01 s behind Node #7) |
| `main.velt` | idiomatic single-threaded Velt in the Node #1 shape (one running sum per row) |
| `main_mt.velt` | `main.velt` with each product split into `availableParallelism() * 4` row blocks: `spawn` + `Promise.all` |
| `main_opt.velt` | single-threaded, the Rust #5 shape: two rows at a time, two lanes each in a Copy struct `F64x2` |

The official files are unchanged apart from a first `// Source:` line (`_st.rs`: three lines).
Naming: `main.go` / `main.js` are the fastest published programs (parallel); `main_st.*` the
fastest single-threaded ones; Rust `_st` is the reference for single-threaded Velt.

## Deviations
- `main_opt.velt` (like Rust #5) needs an even N.

## Correctness
Every implementation prints the official output at N = 100 and the Rust output (`1.274224153`)
at N = 5500, both Velt backends included.

## Where the time goes

Timings are in [../RESULTS.md](../RESULTS.md).

- **`main.velt` (1.75× Rust CPU) and `main_mt.velt`: one dependency chain per row.** The
  Node #1 shape sums each row into one accumulator, so the loop runs at the latency of a
  dependent `fadd` chain. Rust #5 computes two rows at once, each in two lanes (even/odd `j`):
  four independent sums, which LLVM turns into `fdiv.2d`. A Rust program with exactly the
  `main.velt` loops runs at parity with it; `main_opt.velt` (the Rust #5 shape) is within 1.18×
  of Rust. LLVM hoists the bounds check of `u[j]` out of the loop (loop versioning), so no bounds
  checks remain in the hot loop.
- **`/ 2` on `i64`.** `(i + j) * (i + j + 1)` can wrap, so a range analysis cannot prove it
  non-negative, but the product of two values that differ by an odd constant is always even;
  `velt_opt::divisions` uses that to turn the exact `/ 2` into an arithmetic shift.
- **`main_mt.velt` peak RSS** grows with the worker count, not with N (each task allocates a
  copy of the input vector and a result array on its worker thread, and mimalloc keeps freed
  pages per thread).
- **Cranelift** does not vectorize the `F64x2` lanes (`main_opt.velt` 1.2× LLVM, `main.velt`
  1.5×).
