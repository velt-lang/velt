# Benchmarks Game results: Velt vs Rust, Go, Node, Bun

Machine: Apple M4 (4 performance + 6 efficiency cores), 32 GB, macOS 26.6.2. Toolchains:
- Velt `--release`; the LLVM backend goes through Apple clang 21.0.0.
- rustc 1.98.1: release, LTO, 1 codegen unit, `-C target-cpu=native`.
- Go 1.27.1, Node 24.11.1, Bun 1.4.2.

Produced by `bench/benchmarks-game/run.sh --runs 3`. Each implementation first runs once untimed,
and that run's output must match the Rust output byte for byte (every implementation passes
except Bun `fannkuch-redux/main_st.js`, see its NOTES.md). Then it runs three timed times. Wall
time, CPU time (user + sys) and peak RSS are the best of those three runs, and the rusage comes
from `wait4`. Inputs are the official sizes.

Implementations: Rust `<program>` is the fastest published Rust that builds on arm64. Rust
`<program>_st` is the single-thread reference: the same program on a 1-thread rayon pool, or the
fastest single-threaded program where one is published. Velt `main` is the idiomatic
TypeScript-style port, `main_mt` uses spawn + `Promise.all`, and `main_opt` / `main_arena` /
`main_limbs` are variants written to test the compiler or a std building block (see each
program's NOTES.md). Sources and credits are in each program's NOTES.md.

The results come from two runs, both before 2026-10-01 (compiler commit not recorded):
- **Run A** (machine idle) measured every implementation. Its Velt rows for binary-trees, fasta,
  k-nucleotide, mandelbrot, pidigits, regex-redux and reverse-complement are superseded by run B
  and not shown.
- **Run B** (load average 10–20, so absolute times are inflated) measured Velt and Rust for those
  seven programs after the std and runtime work they depend on (`Map` lookups, buffered line
  reading, `Buffer` bulk operations, `stdout.write`, a runtime-backed `BigInt`, `Arena`, regex
  matching without capture tracking). Its "×" columns are against the Rust program timed in the
  same run.

## Summary: Velt LLVM vs Rust

| program | run | Velt `main` × Rust 1-thread CPU | best Velt variant × | Velt `main_mt` wall × Rust all-core | peak RSS MB Rust 1-thread / Velt `main` |
|---|---|---:|---:|---:|---:|
| binary-trees | B | 3.43 | `main_arena` 0.94 | 1.71 | 162 / 160 |
| fannkuch-redux | A | 1.44 | `main_mt` 1.06 | 1.10 | 1.6 / 2.0 |
| fasta | B | 1.22 | `main_mt` 1.13 | 1.22 | 1.5 / 2.4 |
| k-nucleotide | B | 2.31 | — | 4.81 | 130 / 252 |
| mandelbrot | B | 5.24 | `main_opt` 1.55 | 3.77 | 32 / 65 |
| n-body | A | 1.71 | — | (single-threaded) | 1.4 / 2.0 |
| pidigits | B | 2.76 (against GMP) | — | (single-threaded) | 3.0 / 3.5 |
| regex-redux | B | 1.09 | — | 0.79 | 201 / 239 |
| reverse-complement | B | 1.54 | — | 5.39 | 125 / 581 |
| spectral-norm | A | 1.75 | `main_opt` 1.18 | 1.60 | 1.8 / 2.4 |

Where the gaps come from:
- **Loop shape, not the compiler:** fannkuch-redux, mandelbrot, spectral-norm and n-body `main`
  use the simpler loop shape of the Node and Go programs; a Rust program with the same loops
  runs at parity or slower ([../FINDINGS.md](../FINDINGS.md) §2). With the Rust program's shape,
  fannkuch-redux, spectral-norm and mandelbrot are within 1.06–1.55×; n-body `main_opt` (1.85×)
  is not faster than `main`.
- **binary-trees** allocates every node; on an `Arena` pool from `velt/mem` (`main_arena`, like
  Rust #5 with bumpalo) it is faster than the single-threaded Rust.
- **pidigits** `main` runs the Node program on a `BigInt`; the Rust program uses GMP. Run B
  measured an earlier, runtime-backed `BigInt`; `velt/math`'s `BigInt` (Velt code on 64-bit
  limbs) has not been measured in this suite.
- **reverse-complement** holds the whole input and the whole output (2 × 250 MB); Rust reverses
  in place.
- **Cranelift** release builds are up to 3.3× slower than LLVM (k-nucleotide `main`, mandelbrot
  `main_mt`), most rows 1.5–2.5×: no vectorization and no loop-invariant code motion. regex-redux
  and pidigits `main`, where the time is spent in the runtime, and reverse-complement `main_mt`
  are as fast or faster with Cranelift.

## Run B: Velt and Rust after the std and runtime work

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

## Run A: all implementations

| program | implementation | wall s | CPU s | peak RSS MB | wall × Rust | CPU × Rust 1-thread |
|---|---|---:|---:|---:|---:|---:|
| binary-trees | Rust binary-trees | 0.817 | 3.453 | 259.6 | 1.00 |  |
| binary-trees | Rust binary-trees_box | 7.087 | 6.798 | 159.9 | 8.67 |  |
| binary-trees | Rust binary-trees_st | 2.705 | 2.664 | 165.9 | 3.31 |  |
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
| fasta | Go main | 1.717 | 3.584 | 13.5 | 2.04 | 1.73 |
| fasta | Go main_st | 4.056 | 4.002 | 3.9 | 4.82 | 1.93 |
| fasta | Node main | 1.529 | 4.905 | 105.8 | 1.82 | 2.36 |
| fasta | Bun main | 1.376 | 4.289 | 69.3 | 1.63 | 2.06 |
| k-nucleotide | Rust k-nucleotide | 0.838 | 3.062 | 134.2 | 1.00 |  |
| k-nucleotide | Rust k-nucleotide_st | 1.922 | 1.911 | 129.7 | 2.29 |  |
| k-nucleotide | Go main | 4.222 | 20.357 | 242.1 | 5.04 | 10.65 |
| k-nucleotide | Node main | 15.649 | 44.738 | 436.8 | 18.67 | 23.41 |
| k-nucleotide | Bun main | 11.266 | 27.544 | 399.4 | 13.44 | 14.41 |
| mandelbrot | Rust mandelbrot | 0.734 | 3.769 | 32.6 | 1.00 |  |
| mandelbrot | Rust mandelbrot_st | 2.999 | 2.961 | 32.3 | 4.09 |  |
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
| pidigits | Go main | 0.541 | 0.531 | 6.0 | 0.88 | 0.87 |
| pidigits | Node main | 9.918 | 9.676 | 180.0 | 16.13 | 15.78 |
| pidigits | Bun main | 1.439 | 1.718 | 105.0 | 2.34 | 2.80 |
| regex-redux | Rust regex-redux | 0.705 | 0.809 | 201.1 | 1.00 |  |
| regex-redux | Rust regex-redux_st | 0.746 | 0.740 | 200.9 | 1.06 |  |
| regex-redux | Go main | 13.140 | 42.104 | 397.6 | 18.64 | 56.90 |
| regex-redux | Node main | 1.942 | 2.572 | 1048.9 | 2.75 | 3.48 |
| regex-redux | Bun main | 0.611 | 0.776 | 439.0 | 0.87 | 1.05 |
| reverse-complement | Rust reverse-complement | 0.210 | 0.301 | 125.2 | 1.00 |  |
| reverse-complement | Rust reverse-complement_st | 0.276 | 0.266 | 124.9 | 1.31 |  |
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
