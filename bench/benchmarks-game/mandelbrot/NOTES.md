# mandelbrot

> The timing tables below were measured while other agents shared the machine. The authoritative
> numbers are the serial run in `../RESULTS.md`.

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
| `main.vlt` | idiomatic single-threaded Velt: one pixel at a time with early exit, bits packed into a `u8[]`, written with `std/fs_stream` |
| `main_mt.vlt` | `main.vlt` with blocks of 16 rows rendered by spawned tasks, collected with `Promise.all` |
| `main_opt.vlt` | the Rust #4 shape on one thread: 8 pixels at a time in a Copy struct `F64x8` |

The official files are unchanged apart from a first `// Source:` line (`_st.rs` and `main.go`:
see below). Naming as in spectral-norm: `main.*` fastest published, `main_st.*` fastest
single-threaded, Rust `_st` = the reference for single-threaded Velt.

## Deviations
- `main.go`: on arm64 the Go compiler fuses `inv*float64(xy) - 1.5` into a fused multiply-add,
  the initial coordinates change in the last bit and the unmodified Go #4 prints a different
  image (1 byte differs at N = 200 and at N = 16000; checked). The official site runs x86-64,
  where Go does not fuse. `main.go` wraps that product in `float64(...)`, the Go spec's way to
  forbid fusion; the inner loop is untouched (its fused ops do not change the output).
- Binary output: Velt has no `process.stdout.write(bytes)`; the programs write the header with
  `FileWriter.write` and the bitmap with `writeBytes` on `await openWrite("/dev/stdout")`
  (`std/fs_stream`, from std-net). This works with stdout redirected to a file, a pipe and
  `/dev/null` (all three checked); `/dev/stdout` is POSIX-only.

## Correctness
Every implementation prints the official image at N = 200 and the Rust image at N = 16000
(byte-for-byte), both Velt backends included. The Velt programs also handle sizes that are not a
multiple of 8 (N = 201: same bytes as a scratch Rust port and Go #8).

## Timings (Apple M4, 10 cores: 4P + 6E, 32 GB, macOS)

Versions and conditions as in n-body/NOTES.md (best of two rounds of 3 runs, runs over 20 s once
per round; loaded, shared machine: wall times of the long single-threaded runs vary most).

| implementation | threads | wall s | CPU s | peak RSS MB | wall × Rust | CPU × Rust |
|---|---|---:|---:|---:|---:|---:|
| Rust `mandelbrot.rs` | all | 0.79 | 3.67 | 32.5 | 1.00 | 1.00 |
| Rust `mandelbrot_st.rs` | 1 | 3.08 | 3.02 | 32.3 | 1.00 | 1.00 |
| Velt LLVM `main.vlt` | 1 | 13.08 | 12.80 | 72.2 | 4.25 | 4.24 |
| Velt LLVM `main_mt.vlt` | all | 3.14 | 16.74 | 36.7 | 3.97 | 4.56 |
| Velt LLVM `main_opt.vlt` | 1 | 3.52 | 3.44 | 72.3 | 1.14 | 1.14 |
| Velt Cranelift `main.vlt` | 1 | 25.05 | 23.51 | 72.2 | 8.13 | 7.78 |
| Velt Cranelift `main_mt.vlt` | all | 5.68 | 24.75 | 36.7 | 7.19 | 6.74 |
| Velt Cranelift `main_opt.vlt` | 1 | 10.43 | 7.66 | 72.3 | 3.39 | 2.54 |
| Go `main.go` | 2 × CPUs | 2.64 | 13.85 | 39.3 | 3.34 | 3.77 |
| Go `main_st.go` | 1 | 20.79 | 16.50 | 3.9 | 6.75 | 5.46 |
| Node `main.js` | all | 3.83 | 14.63 | 184.0 | 4.85 | 3.99 |
| Bun `main.js` | all | 2.95 | 11.47 | 108.6 | 3.73 | 3.13 |
| Node `main_st.js` | 1 | 24.62 | 17.91 | 57.5 | 7.99 | 5.93 |
| Bun `main_st.js` | 1 | 16.23 | 14.64 | 42.4 | 5.27 | 4.85 |

## Gaps (Velt LLVM > 1.2× Rust)

### `main.vlt` 4.2× and `main_mt.vlt` 4.0×: scalar per-pixel loop, not the compiler
`main.vlt` iterates one pixel at a time and leaves the loop as soon as it escapes; that is the
shape of the simple published programs (Go #8, Node #2). Rust #4 iterates 8 pixels in
lock-step (4 × `fmul.2d` per multiply on NEON), tests for escape only every 4 iterations and only
when the previous byte was all-escaped. A scratch Rust program with exactly the `main.vlt` loops
took 14.99 s CPU against 15.39 s for `main.vlt` in the same session: parity for the same shape.

`main_opt.vlt` writes the Rust #4 shape in Velt with a Copy struct of eight `f64` lanes
(`F64x8 { l0 … l7 }` with `add`/`sub`/`mul` methods). LLVM vectorizes it like Rust's `[f64; 8]`
(68 `fmul.2d` in the binary against 76 for Rust) and it runs within 1.14× of Rust
single-threaded: Copy structs of lanes are a usable SIMD idiom in Velt today.

`main_mt.vlt` uses 31 % more CPU than `main.vlt` (Rust: 21 % more for mt than st): with all 10
cores busy, the 6 efficiency cores run the same rows more slowly.

### Peak RSS 72 MB for `main.vlt` / `main_opt.vlt` (Rust 32 MB): `writeBytes` copies the array
The bitmap is 32 MB. `FileWriter.writeBytes(data: u8[])` hands the array to
`velt_rt_fs_writer_write_bytes`, which copies it before moving the write to the blocking pool
(`crates/velt_rt/src/fs/ops.rs`: `data_arg` = `(*s).as_bytes().to_vec()`), so the whole image
exists twice. `main_mt.vlt` writes 1000 blocks of 32 KB and peaks at 36.7 MB.

Proposed fix (runtime + std): the async `writeBytes` already owns its argument (async params are
owned), so it can pass ownership instead of a borrow: a `velt_rt_fs_writer_write_bytes_owned`
that takes the array's buffer (pointer, length, capacity) and frees it with the Velt allocator
after the write. Same for `write(string)` of large strings.

### Cranelift
`main.vlt` 1.8× LLVM, `main_opt.vlt` 2.2× LLVM: Cranelift does not vectorize the 8 lanes
(bench/RESULTS.md: no auto-vectorization).

## Friction for a TypeScript developer
- Binary stdout needs `openWrite("/dev/stdout")` and an `async main` (no
  `process.stdout.write(Uint8Array)`), and `/dev/stdout` does not exist on Windows.
- `Math.min` / `Math.max` are `f64`-only: `Math.min(y + 16, size)` on `i64` needs casts or a
  ternary (used in `main_mt.vlt`).
- `as` binds looser than `*` and `/` (as in TypeScript), so `2.0 * y as f64` does not type-check;
  the conversions need parentheses: `(2.0 * (y as f64)) / (size as f64)`.
- No `Uint8Array(n)`: the bitmap is a `u8[]` grown by `push`.

## Pending goldens
No wrong output or crash was found. Filed: `tests/golden/bugs/std_math_min_integers`
(`Math.min` / `Math.max` on integers).

## Std and runtime round
Every variant writes the bitmap with `stdout.write`, straight from the array (no copy, no
`/dev/stdout`). Peak RSS went from 72 to 65 MB; the time gap is the algorithm (FINDINGS §8.9).
