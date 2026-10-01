# Benchmarks Game results: Velt vs Rust, Go, Node, Bun

Machine: Apple M4 (4 performance + 6 efficiency cores), 32 GB, macOS 26.6.2, and nothing else
running. Toolchains:
- Velt 0.1.0 (pre-release development build, with the std networking modules), `--release`. The LLVM
  backend goes through Apple clang 21.0.0.
- rustc 1.98.1: release, LTO, 1 codegen unit, `-C target-cpu=native`.
- Go 1.27.1, Node 24.11.1, Bun 1.4.2.

Produced by `bench/benchmarks-game/run.sh --runs 3`. Each implementation first runs once untimed,
and that run's output must match the Rust output byte for byte (every implementation passed except
Bun `fannkuch-redux/main_st.js`, see its NOTES.md). Then it runs three timed times. Wall time, CPU
time (user + sys) and peak RSS are the best of those three runs, and the rusage comes from
`wait4`. Inputs are the official sizes. The quick sizes (`--quick`) all match the official
output files.

Implementations: Rust `<program>` is the fastest published Rust that builds on arm64. Rust
`<program>_st` is the single-thread reference: the same program on a 1-thread rayon pool, or the
fastest single-threaded program where one is published. Velt `main` is the idiomatic
TypeScript-style port, `main_mt` uses spawn + `Promise.all`, and `main_opt` is an optimized
variant written to test the compiler (see each program's NOTES.md). Sources and credits are in
each program's NOTES.md.

## Summary: Velt LLVM vs Rust

| program | Rust 1-thread CPU s | Velt `main` × | best Velt variant × | Rust all-core wall s | Velt `main_mt` wall × | peak RSS MB Rust / Velt `main` |
|---|---:|---:|---:|---:|---:|---:|
| binary-trees | 2.66 | 3.23 (1.27 vs Rust Box+mimalloc) | — | 0.82 | 2.71 | 166 / 160 |
| fannkuch-redux | 26.48 | 1.44 | `main_mt` 1.06 (CPU) | 5.78 | 1.10 | 1.6 / 2.0 |
| fasta | 2.08 | 1.95 | — | 0.84 | 1.86 | 1.5 / 3.7 |
| k-nucleotide | 1.91 | 3.07 | — | 0.84 | 4.39 | 130 / 246 |
| mandelbrot | 2.96 | 4.88 | `main_opt` 1.12 | 0.73 | 5.28 | 32 / 72 |
| n-body | 1.69 | 1.71 | `main_opt` 1.85 | (single-threaded) | — | 1.4 / 2.0 |
| pidigits | 0.61 (GMP) | 3.53 (1.19 vs the same limb code in Rust) | — | (single-threaded) | — | 3.0 / 2.6 |
| regex-redux | 0.74 | 1.44 | — | 0.71 | 1.43 | 201 / 225 |
| reverse-complement | 0.27 | 4.66 | — | 0.21 | 4.67 | 125 / 422 |
| spectral-norm | 0.44 | 1.75 | `main_opt` 1.18 | 0.11 | 1.60 | 1.8 / 2.4 |

The "×" columns are Velt divided by Rust: CPU time against the single-threaded Rust, and wall time
against the fastest Rust. Every program exceeds 1.2× in its idiomatic form; `bench/FINDINGS.md`
section 8 has the root causes. In short:
- **Algorithm, not the compiler:** fannkuch-redux, mandelbrot, spectral-norm and n-body `main`
  use the simpler loop shape that the Node and Go programs use. A Rust program with the same loops
  runs at parity or slower. With the Rust program's shape, Velt is within 1.06–1.18×.
- **Compiler, runtime and std gaps:**
  - reverse-complement: locals of an async function live in its frame, and there are no bulk
    `u8[]` operations.
  - k-nucleotide: the `Map` hit path, and line reading.
  - fasta: no `noalias` on class-typed parameters, and the float algorithm.
  - regex-redux: the runtime iterates captures for every match.
  - binary-trees: allocator wrappers, and no arena.
  - pidigits: no `BigInt`, and 32-bit limbs.
  - n-body `main_opt`: arrays filled by push in a loop stay in memory.
- **Cranelift** release builds are 1.1–2.5× slower than LLVM. It does no vectorization and no
  loop-invariant code motion (`bench/RESULTS.md`). The exception is regex-redux, where the time is
  spent in the runtime and Cranelift is slightly faster.

## Std and runtime round

FINDINGS §8.4–8.6 (std and runtime only; no compiler changes). The machine was busy during this
round (load average 10–20), so absolute times are inflated and noisy. Each "×" below is against
the Rust program timed in the same run, and the last column gives the change measured as CPU
cycles (`/usr/bin/time -l`) against `origin/main`, which the load doesn't skew. All outputs matched
Rust's byte for byte.

| program | Velt LLVM variant | CPU × Rust 1-thread (before → after the std/runtime round) | peak RSS MB (before → after) | cycles vs origin/main | what changed |
|---|---|---:|---:|---:|---|
| regex-redux | `main` | 1.44 → 1.09 | 225 → 239 | −15–20% | `find_at` fast paths, `readAll` without the second copy |
| k-nucleotide | `main` | 3.07 → 2.31 | 246 → 252 | −8% | `Map` hit returns the entry; buffered `readLineSync` |
| reverse-complement | `main` | 4.66 → 1.54 | 422 → 581 | −68% | rewritten on `readAllBytesSync`, `indexOf`, `Buffer.alloc`, `stdout.write` |
| fasta | `main` | 1.95 → 1.22 | 3.7 → 2.4 | −3% | `stdout.write` instead of `openWrite("/dev/stdout")` |
| mandelbrot | `main` | 4.88 → 5.24 | 72 → 65 | ±0 | `stdout.write` (the gap is §8.9, the algorithm) |
| pidigits | `main` | 3.53 → 2.76 | 2.6 → 3.5 | −39% | the Node program on `std/bigint` (dashu-int) |
| pidigits | `main_limbs` | — → 3.56 | 2.7 | ±0 | the round-5 limb class, kept for comparison |
| binary-trees | `main_arena` | — → 0.94 | 98 | −73% vs `main` | new: nodes in a `std/arena` pool, like Rust #5 (bumpalo) |

Reading the table:
- The fasta ratio fell further than its cycles did because the earlier Rust row was measured on an
  idle machine; compare the cycle column.
- reverse-complement `main` holds the whole input and the whole output (2 × 250 MB). Rust
  reverses in place (125 MB).
- binary-trees `main_arena` is compared with the single-threaded Rust `binary-trees_st`, which
  allocates per node.

Full rows of this run:

| program | implementation | wall s | CPU s | peak RSS MB | wall × Rust | CPU × Rust 1-thread |
|---|---|---:|---:|---:|---:|---:|
| regex-redux | Rust regex-redux | 0.579 | 0.683 | 201.2 | 1.00 |  |
| regex-redux | Rust regex-redux_st | 0.528 | 0.525 | 201.0 | 0.91 |  |
| regex-redux | Velt LLVM main | 0.575 | 0.573 | 239.0 | 0.99 | 1.09 |
| regex-redux | Velt Cranelift main | 0.495 | 0.493 | 253.4 | 0.85 | 0.94 |
| regex-redux | Velt LLVM main_mt | 0.456 | 0.524 | 326.0 | 0.79 | 1.00 |
| regex-redux | Velt Cranelift main_mt | 0.479 | 0.543 | 326.1 | 0.83 | 1.03 |
| k-nucleotide | Rust k-nucleotide | 0.742 | 2.787 | 134.4 | 1.00 |  |
| k-nucleotide | Rust k-nucleotide_st | 2.738 | 2.265 | 129.8 | 3.69 |  |
| k-nucleotide | Velt LLVM main | 5.237 | 5.232 | 251.8 | 7.06 | 2.31 |
| k-nucleotide | Velt Cranelift main | 15.739 | 12.578 | 251.8 | 21.21 | 5.55 |
| k-nucleotide | Velt LLVM main_mt | 3.568 | 7.771 | 1017.5 | 4.81 | 3.43 |
| k-nucleotide | Velt Cranelift main_mt | 5.974 | 13.473 | 1012.4 | 8.05 | 5.95 |
| reverse-complement | Rust reverse-complement | 0.217 | 0.288 | 125.2 | 1.00 |  |
| reverse-complement | Rust reverse-complement_st | 0.262 | 0.243 | 124.9 | 1.21 |  |
| reverse-complement | Velt LLVM main | 0.399 | 0.374 | 580.6 | 1.84 | 1.54 |
| reverse-complement | Velt Cranelift main | 0.748 | 0.724 | 580.6 | 3.45 | 2.98 |
| reverse-complement | Velt LLVM main_mt | 1.170 | 1.426 | 490.6 | 5.39 | 5.87 |
| reverse-complement | Velt Cranelift main_mt | 0.967 | 1.512 | 439.2 | 4.46 | 6.22 |
| fasta | Rust fasta | 1.134 | 1.444 | 2.1 | 1.00 |  |
| fasta | Rust fasta_st | 2.777 | 2.714 | 1.5 | 2.45 |  |
| fasta | Velt LLVM main | 3.392 | 3.323 | 2.4 | 2.99 | 1.22 |
| fasta | Velt Cranelift main | 3.477 | 3.453 | 2.4 | 3.07 | 1.27 |
| fasta | Velt LLVM main_mt | 1.386 | 3.065 | 26.4 | 1.22 | 1.13 |
| fasta | Velt Cranelift main_mt | 1.840 | 4.605 | 32.5 | 1.62 | 1.70 |
| mandelbrot | Rust mandelbrot | 0.535 | 3.731 | 32.6 | 1.00 |  |
| mandelbrot | Rust mandelbrot_st | 2.714 | 2.696 | 32.3 | 5.07 |  |
| mandelbrot | Velt LLVM main | 14.688 | 14.124 | 64.8 | 27.45 | 5.24 |
| mandelbrot | Velt Cranelift main | 33.358 | 27.287 | 64.7 | 62.35 | 10.12 |
| mandelbrot | Velt LLVM main_mt | 2.019 | 15.485 | 38.0 | 3.77 | 5.74 |
| mandelbrot | Velt Cranelift main_mt | 6.619 | 26.900 | 38.0 | 12.37 | 9.98 |
| mandelbrot | Velt LLVM main_opt | 7.337 | 4.187 | 64.8 | 13.71 | 1.55 |
| mandelbrot | Velt Cranelift main_opt | 12.337 | 8.429 | 64.9 | 23.06 | 3.13 |
| pidigits | Rust pidigits | 0.716 | 0.628 | 3.0 | 1.00 |  |
| pidigits | Rust pidigits_limbs | 2.011 | 2.000 | 2.0 | 2.81 |  |
| pidigits | Velt LLVM main | 1.865 | 1.735 | 3.5 | 2.60 | 2.76 |
| pidigits | Velt Cranelift main | 1.262 | 1.257 | 3.5 | 1.76 | 2.00 |
| pidigits | Velt LLVM main_limbs | 2.253 | 2.238 | 2.7 | 3.15 | 3.56 |
| pidigits | Velt Cranelift main_limbs | 5.662 | 5.582 | 2.7 | 7.91 | 8.89 |
| binary-trees | Rust binary-trees | 0.539 | 3.452 | 217.3 | 1.00 |  |
| binary-trees | Rust binary-trees_box | 5.947 | 5.892 | 159.9 | 11.03 |  |
| binary-trees | Rust binary-trees_st | 1.382 | 1.380 | 161.9 | 2.56 |  |
| binary-trees | Velt LLVM main | 4.750 | 4.733 | 159.6 | 8.81 | 3.43 |
| binary-trees | Velt Cranelift main | 10.252 | 10.152 | 159.6 | 19.02 | 7.36 |
| binary-trees | Velt LLVM main_arena | 1.303 | 1.301 | 98.1 | 2.42 | 0.94 |
| binary-trees | Velt Cranelift main_arena | 2.836 | 2.834 | 98.1 | 5.26 | 2.05 |
| binary-trees | Velt LLVM main_mt | 0.920 | 6.504 | 158.2 | 1.71 | 4.71 |
| binary-trees | Velt Cranelift main_mt | 2.210 | 16.379 | 158.1 | 4.10 | 11.87 |

## All implementations

| program | implementation | wall s | CPU s | peak RSS MB | wall × Rust | CPU × Rust 1-thread |
|---|---|---:|---:|---:|---:|---:|
| binary-trees | Rust binary-trees | 0.817 | 3.453 | 259.6 | 1.00 |  |
| binary-trees | Rust binary-trees_box | 7.087 | 6.798 | 159.9 | 8.67 |  |
| binary-trees | Rust binary-trees_st | 2.705 | 2.664 | 165.9 | 3.31 |  |
| binary-trees | Velt LLVM main | 8.812 | 8.616 | 159.5 | 10.79 | 3.23 |
| binary-trees | Velt Cranelift main | 17.110 | 16.739 | 159.5 | 20.94 | 6.28 |
| binary-trees | Velt LLVM main_mt | 2.210 | 9.850 | 158.1 | 2.71 | 3.70 |
| binary-trees | Velt Cranelift main_mt | 4.208 | 18.177 | 158.1 | 5.15 | 6.82 |
| binary-trees | Go main | 5.641 | 27.163 | 548.8 | 6.90 | 10.20 |
| binary-trees | Go main_st | 10.525 | 23.968 | 226.0 | 12.88 | 9.00 |
| binary-trees | Node main | 2.570 | 11.841 | 1407.9 | 3.15 | 4.44 |
| binary-trees | Bun main | 2.766 | 12.634 | 1169.7 | 3.39 | 4.74 |
| binary-trees | Node main_st | 10.636 | 14.872 | 1157.3 | 13.02 | 5.58 |
| binary-trees | Bun main_st | 6.568 | 7.769 | 1024.0 | 8.04 | 2.92 |
| fannkuch-redux | Rust fannkuch-redux | 5.781 | 26.049 | 1.8 | 1.00 |  |
| fannkuch-redux | Rust fannkuch-redux_st | 27.475 | 26.478 | 1.6 | 4.75 |  |
| fannkuch-redux | Velt LLVM main | 39.538 | 38.063 | 2.0 | 6.84 | 1.44 |
| fannkuch-redux | Velt Cranelift main | 69.066 | 67.366 | 2.0 | 11.95 | 2.54 |
| fannkuch-redux | Velt LLVM main_mt | 6.354 | 28.190 | 3.6 | 1.10 | 1.06 |
| fannkuch-redux | Velt Cranelift main_mt | 12.632 | 53.311 | 3.6 | 2.19 | 2.01 |
| fannkuch-redux | Go main | 7.590 | 25.512 | 4.3 | 1.31 | 0.96 |
| fannkuch-redux | Go main_st | 37.739 | 36.945 | 3.9 | 6.53 | 1.40 |
| fannkuch-redux | Node main | 8.876 | 41.846 | 147.3 | 1.54 | 1.58 |
| fannkuch-redux | Bun main | 6.500 | 30.611 | 86.9 | 1.12 | 1.16 |
| fannkuch-redux | Node main_st | 45.679 | 43.662 | 48.1 | 7.90 | 1.65 |
| fannkuch-redux | Bun main_st | n/a | | | | |
| fasta | Rust fasta | 0.842 | 1.152 | 2.1 | 1.00 |  |
| fasta | Rust fasta_st | 2.115 | 2.077 | 1.5 | 2.51 |  |
| fasta | Velt LLVM main | 4.237 | 4.046 | 3.7 | 5.03 | 1.95 |
| fasta | Velt Cranelift main | 4.605 | 4.410 | 3.7 | 5.47 | 2.12 |
| fasta | Velt LLVM main_mt | 1.567 | 3.292 | 32.7 | 1.86 | 1.58 |
| fasta | Velt Cranelift main_mt | 1.771 | 4.320 | 36.4 | 2.10 | 2.08 |
| fasta | Go main | 1.717 | 3.584 | 13.5 | 2.04 | 1.73 |
| fasta | Go main_st | 4.056 | 4.002 | 3.9 | 4.82 | 1.93 |
| fasta | Node main | 1.529 | 4.905 | 105.8 | 1.82 | 2.36 |
| fasta | Bun main | 1.376 | 4.289 | 69.3 | 1.63 | 2.06 |
| k-nucleotide | Rust k-nucleotide | 0.838 | 3.062 | 134.2 | 1.00 |  |
| k-nucleotide | Rust k-nucleotide_st | 1.922 | 1.911 | 129.7 | 2.29 |  |
| k-nucleotide | Velt LLVM main | 5.924 | 5.871 | 246.0 | 7.07 | 3.07 |
| k-nucleotide | Velt Cranelift main | 11.327 | 10.988 | 246.1 | 13.52 | 5.75 |
| k-nucleotide | Velt LLVM main_mt | 3.681 | 8.284 | 1009.7 | 4.39 | 4.33 |
| k-nucleotide | Velt Cranelift main_mt | 5.585 | 14.023 | 1016.4 | 6.66 | 7.34 |
| k-nucleotide | Go main | 4.222 | 20.357 | 242.1 | 5.04 | 10.65 |
| k-nucleotide | Node main | 15.649 | 44.738 | 436.8 | 18.67 | 23.41 |
| k-nucleotide | Bun main | 11.266 | 27.544 | 399.4 | 13.44 | 14.41 |
| mandelbrot | Rust mandelbrot | 0.734 | 3.769 | 32.6 | 1.00 |  |
| mandelbrot | Rust mandelbrot_st | 2.999 | 2.961 | 32.3 | 4.09 |  |
| mandelbrot | Velt LLVM main | 14.709 | 14.463 | 72.2 | 20.04 | 4.88 |
| mandelbrot | Velt Cranelift main | 22.648 | 22.264 | 72.2 | 30.86 | 7.52 |
| mandelbrot | Velt LLVM main_mt | 3.873 | 17.795 | 36.7 | 5.28 | 6.01 |
| mandelbrot | Velt Cranelift main_mt | 5.404 | 26.585 | 36.7 | 7.36 | 8.98 |
| mandelbrot | Velt LLVM main_opt | 3.350 | 3.320 | 72.3 | 4.56 | 1.12 |
| mandelbrot | Velt Cranelift main_opt | 7.092 | 6.967 | 72.3 | 9.66 | 2.35 |
| mandelbrot | Go main | 2.394 | 13.553 | 39.2 | 3.26 | 4.58 |
| mandelbrot | Go main_st | 14.706 | 14.380 | 4.0 | 20.04 | 4.86 |
| mandelbrot | Node main | 3.230 | 15.318 | 189.2 | 4.40 | 5.17 |
| mandelbrot | Bun main | 2.534 | 11.926 | 109.7 | 3.45 | 4.03 |
| mandelbrot | Node main_st | 17.292 | 16.938 | 57.7 | 23.56 | 5.72 |
| mandelbrot | Bun main_st | 14.259 | 14.030 | 41.8 | 19.43 | 4.74 |
| n-body | Rust n-body | 1.708 | 1.689 | 1.4 | 1.00 |  |
| n-body | Velt LLVM main | 2.958 | 2.887 | 2.0 | 1.73 | 1.71 |
| n-body | Velt Cranelift main | 5.991 | 5.784 | 2.0 | 3.51 | 3.42 |
| n-body | Velt LLVM main_opt | 3.181 | 3.120 | 2.1 | 1.86 | 1.85 |
| n-body | Velt Cranelift main_opt | 6.035 | 5.981 | 2.1 | 3.53 | 3.54 |
| n-body | Go main | 2.887 | 2.815 | 3.9 | 1.69 | 1.67 |
| n-body | Node main | 3.961 | 3.879 | 50.6 | 2.32 | 2.30 |
| n-body | Bun main | 3.992 | 3.937 | 24.9 | 2.34 | 2.33 |
| pidigits | Rust pidigits | 0.615 | 0.613 | 3.0 | 1.00 |  |
| pidigits | Rust pidigits_limbs | 1.844 | 1.826 | 2.0 | 3.00 |  |
| pidigits | Velt LLVM main | 2.189 | 2.166 | 2.6 | 3.56 | 3.53 |
| pidigits | Velt Cranelift main | 5.500 | 5.414 | 2.6 | 8.94 | 8.83 |
| pidigits | Go main | 0.541 | 0.531 | 6.0 | 0.88 | 0.87 |
| pidigits | Node main | 9.918 | 9.676 | 180.0 | 16.13 | 15.78 |
| pidigits | Bun main | 1.439 | 1.718 | 105.0 | 2.34 | 2.80 |
| regex-redux | Rust regex-redux | 0.705 | 0.809 | 201.1 | 1.00 |  |
| regex-redux | Rust regex-redux_st | 0.746 | 0.740 | 200.9 | 1.06 |  |
| regex-redux | Velt LLVM main | 1.080 | 1.066 | 225.0 | 1.53 | 1.44 |
| regex-redux | Velt Cranelift main | 0.991 | 0.980 | 224.9 | 1.41 | 1.32 |
| regex-redux | Velt LLVM main_mt | 1.006 | 1.140 | 513.8 | 1.43 | 1.54 |
| regex-redux | Velt Cranelift main_mt | 0.899 | 1.045 | 513.8 | 1.28 | 1.41 |
| regex-redux | Go main | 13.140 | 42.104 | 397.6 | 18.64 | 56.90 |
| regex-redux | Node main | 1.942 | 2.572 | 1048.9 | 2.75 | 3.48 |
| regex-redux | Bun main | 0.611 | 0.776 | 439.0 | 0.87 | 1.05 |
| reverse-complement | Rust reverse-complement | 0.210 | 0.301 | 125.2 | 1.00 |  |
| reverse-complement | Rust reverse-complement_st | 0.276 | 0.266 | 124.9 | 1.31 |  |
| reverse-complement | Velt LLVM main | 1.240 | 1.240 | 421.9 | 5.90 | 4.66 |
| reverse-complement | Velt Cranelift main | 1.431 | 1.423 | 421.9 | 6.81 | 5.35 |
| reverse-complement | Velt LLVM main_mt | 0.981 | 1.336 | 437.0 | 4.67 | 5.02 |
| reverse-complement | Velt Cranelift main_mt | 1.145 | 1.734 | 465.9 | 5.45 | 6.52 |
| reverse-complement | Go main | 0.162 | 0.350 | 143.3 | 0.77 | 1.32 |
| reverse-complement | Go main_st | 0.256 | 0.246 | 161.2 | 1.22 | 0.92 |
| reverse-complement | Node main | 2.667 | 2.735 | 232.8 | 12.70 | 10.28 |
| reverse-complement | Bun main | 2.059 | 2.091 | 358.0 | 9.80 | 7.86 |
| spectral-norm | Rust spectral-norm | 0.110 | 0.800 | 2.2 | 1.00 |  |
| spectral-norm | Rust spectral-norm_st | 0.446 | 0.444 | 1.8 | 4.05 |  |
| spectral-norm | Velt LLVM main | 0.777 | 0.776 | 2.4 | 7.06 | 1.75 |
| spectral-norm | Velt Cranelift main | 1.174 | 1.168 | 2.4 | 10.67 | 2.63 |
| spectral-norm | Velt LLVM main_mt | 0.176 | 1.295 | 27.2 | 1.60 | 2.92 |
| spectral-norm | Velt Cranelift main_mt | 0.513 | 3.208 | 30.0 | 4.66 | 7.23 |
| spectral-norm | Velt LLVM main_opt | 0.528 | 0.525 | 2.4 | 4.80 | 1.18 |
| spectral-norm | Velt Cranelift main_opt | 0.640 | 0.639 | 2.4 | 5.82 | 1.44 |
| spectral-norm | Go main | 0.280 | 0.946 | 5.4 | 2.55 | 2.13 |
| spectral-norm | Go main_st | 2.777 | 2.753 | 4.9 | 25.25 | 6.20 |
| spectral-norm | Node main | 0.694 | 3.247 | 163.1 | 6.31 | 7.31 |
| spectral-norm | Bun main | 0.532 | 2.439 | 79.6 | 4.84 | 5.49 |
| spectral-norm | Node main_st | 1.509 | 1.506 | 50.6 | 13.72 | 3.39 |
| spectral-norm | Bun main_st | 1.720 | 1.712 | 26.8 | 15.64 | 3.86 |

Best of 3 runs (wall and CPU seconds, peak RSS), official sizes.
