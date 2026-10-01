# spectral-norm

> The timing tables below were measured while other agents shared the machine. The authoritative
> numbers are the serial run in `../RESULTS.md`.

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
| `main.vlt` | idiomatic single-threaded Velt in the Node #1 shape (one running sum per row) |
| `main_mt.vlt` | `main.vlt` with each product split into `availableParallelism() * 4` row blocks: `spawn` + `Promise.all` |
| `main_opt.vlt` | single-threaded, the Rust #5 shape: two rows at a time, two lanes each in a Copy struct `F64x2` |

The official files are unchanged apart from a first `// Source:` line (`_st.rs`: three lines).
Naming: `main.go` / `main.js` are the fastest published programs (parallel); `main_st.*` the
fastest single-threaded ones; Rust `_st` is the reference for single-threaded Velt.

## Deviations
- `toFixed` is declared with `extend f64` (Velt has none; see n-body/NOTES.md).
- `main_opt.vlt` (like Rust #5) needs an even N.

## Correctness
Every implementation prints the official output at N = 100 and the Rust output (`1.274224153`)
at N = 5500, both Velt backends included.

## Timings (Apple M4, 10 cores: 4P + 6E, 32 GB, macOS)

Versions and conditions as in n-body/NOTES.md (rustc 1.98.1 native, velt 0.1.0 8f35554 with
Apple clang 21, Go 1.27.1, Node 24.11.1, Bun 1.4.2; best of two rounds of 3 runs on a shared,
loaded machine). "× Rust" compares with the Rust program of the same thread count
(`spectral-norm.rs` for "all", `spectral-norm_st.rs` for "1").

| implementation | threads | wall s | CPU s | peak RSS MB | wall × Rust | CPU × Rust |
|---|---|---:|---:|---:|---:|---:|
| Rust `spectral-norm.rs` | all | 0.16 | 0.83 | 2.2 | 1.00 | 1.00 |
| Rust `spectral-norm_st.rs` | 1 | 0.57 | 0.57 | 1.8 | 1.00 | 1.00 |
| Velt LLVM `main.vlt` | 1 | 1.21 | 1.20 | 2.4 | 2.12 | 2.11 |
| Velt LLVM `main_mt.vlt` | all | 0.29 | 1.46 | 22.8 | 1.81 | 1.76 |
| Velt LLVM `main_opt.vlt` | 1 | 0.64 | 0.64 | 2.4 | 1.12 | 1.12 |
| Velt Cranelift `main.vlt` | 1 | 1.58 | 1.56 | 2.4 | 2.77 | 2.74 |
| Velt Cranelift `main_mt.vlt` | all | 0.78 | 3.39 | 20.5 | 4.88 | 4.08 |
| Velt Cranelift `main_opt.vlt` | 1 | 1.03 | 1.02 | 2.4 | 1.81 | 1.79 |
| Go `main.go` | 4 | 0.42 | 1.47 | 5.2 | 2.62 | 1.77 |
| Go `main_st.go` | 1 | 3.91 | 3.83 | 5.0 | 6.86 | 6.72 |
| Node `main.js` | all | 0.61 | 2.92 | 160.0 | 3.81 | 3.52 |
| Bun `main.js` | all | 0.67 | 2.58 | 79.1 | 4.19 | 3.11 |
| Node `main_st.js` | 1 | 2.52 | 2.47 | 50.6 | 4.42 | 4.33 |
| Bun `main_st.js` | 1 | 2.10 | 2.07 | 26.8 | 3.68 | 3.63 |

## Gaps (Velt LLVM > 1.2× Rust)

### `main.vlt` 2.1× and `main_mt.vlt` 1.8×: one dependency chain per row
The Node #1 shape sums each row into one accumulator, so the loop runs at the latency of a
dependent `fadd` chain. Rust #5 computes two rows at once, each in two lanes (even/odd `j`): four
independent sums, and LLVM turns them into `fdiv.2d`. Measured with scratch programs, CPU s at
N = 5500, two sessions:

| program | CPU s |
|---|---:|
| `main.vlt` | 1.20–1.33 |
| `main.vlt` with `>> 1` instead of `/ 2` in `a(i, j)` | 1.08–1.11 |
| Rust with exactly the `main.vlt` loops (`usize`, `Vec<f64>`) | 1.05–1.06 |
| `main_opt.vlt` (Rust #5 shape) | 0.64 |
| Rust `spectral-norm_st.rs` | 0.57 |

So the shape explains most of the gap; `main_opt.vlt` is within 1.12× of Rust. LLVM does hoist
the bounds check of `u[j]` out of the loop (loop versioning) and vectorizes the index math, so
no bounds checks remain in the hot loop.

The remaining ~10 % of `main.vlt` is `/ 2` on `i64`:

```
    _26 = mul _23, _25
    _27 = div _26, 2_i64        // → LLVM `sdiv i64 %x, 2` (add the sign bit, then shift)
```

Velt integers wrap on overflow, so the multiply carries no `nsw` and LLVM cannot prove the
product non-negative; Rust's version uses `usize`, where `/ 2` is a plain shift. Not a lowering
bug: a TypeScript developer's integers are `i64`. Possible fix: a range analysis in `velt_opt`
for counters that start at 0 and only increase could mark such products non-negative when the
bounds prove no overflow (here `i, j < n` with `n` from `Number(...)` is unbounded, so the
program-side fix `>> 1` is what works today).

`main_mt.vlt` has the same per-row shape; each task also gets its own copy of the input vector
(async parameters are owned: `u.clone()` per task, 44 KB), which is negligible in time.

### `main_mt.vlt` peak RSS 23 MB (Rust 2.2 MB)
It grows with the worker count, not with N: `VELT_THREADS=1` 5.8 MB, 2 → 9.4 MB, 4 → 13.2 MB,
10 → 24.0 MB (N = 5500). A trivial program that spawns 40 tasks peaks at 3.0 MB, so it comes
from what the tasks allocate (a 44 KB input copy and a result array each, on every worker
thread). That is consistent with mimalloc's per-thread heaps (velt_rt's allocator) keeping freed
pages per thread; not investigated further.

### Cranelift
`main.vlt` 1.3× LLVM, `main_opt.vlt` 1.6× LLVM: no vectorization of the `F64x2` lanes.

## Friction for a TypeScript developer
- No `new Array(n).fill(0)` / `Float64Array`: arrays are filled with `push` loops.
- `toFixed` is missing (an `extend f64` helper per file).
- In async code, arrays passed to a spawned task are copied (owned async params); the
  explicit `u.clone()` documents it, a TypeScript developer would expect sharing.
- `x / 2` on integers is exact integer division (`i64`), not JS float division; here that is what
  the algorithm wants, but it costs a sign fix-up compared with `>> 1`.

## Pending goldens
No wrong output or crash was found. The missing preallocating constructors are filed as
`tests/golden/bugs/std_array_new_fill` (see n-body), `toFixed` as `std_number_to_fixed`.
