# n-body: port notes

Official description: https://benchmarksgame-team.pages.debian.net/benchmarksgame/description/nbody.html
(N = 50,000,000; QUICK_N = 1000, `expected-quick.txt` = the official `nbody-output.txt`).

## Sources and credits

| file | source |
|---|---|
| `rust/src/bin/n-body.rs` | Rust #3, Ilia Schelokov: the fastest published Rust without `std::arch::x86_64` intrinsics (#9, #7, #5, #6 are x86-only and do not build on the M4) |
| `main.go` | Go #3, The Go Authors / Christoph Bauer / Isaac Gouy / Antonio Petri (fastest Go) |
| `main.js` | Node.js #6, Isaac Gouy / Andrey Filatkin (fastest Node); Bun runs the same file |
| `main.velt` | idiomatic Velt: a `Body` class, `Body[]`, the Node #6 loop shape |
| `main_opt.velt` | the Rust #3 loop shape with value structs (`Vec3`) in an object type (`Body`), see "Gaps" |

All published n-body programs are single-threaded, so there is no `_mt` variant. The reference
files are unchanged apart from a first `// Source:` line.

## Deviations
- `main.velt` computes `dx * (mass * mag)` (the Rust #3 order) where Node #6 has
  `dx * mass * mag`; the printed energies are identical at QUICK_N and N.

## Correctness
Every implementation (Velt LLVM and Cranelift, both files; Rust; Go; Node; Bun) prints the official
output at N = 1000 and the Rust output at N = 50,000,000 (`-0.169075164` / `-0.169059907`).

## Where the time goes

Timings are in [../RESULTS.md](../RESULTS.md).

- **`main.velt` (1.7× Rust CPU) is the loop shape, not the compiler.** Node #6 (and
  `main.velt`) handles one pair at a time: distance → `sqrt` → divide → velocity update, one
  scalar `sqrt` + `fdiv` per pair on the critical path. Rust #3 first computes all 10 distance
  vectors, then all 10 magnitudes (independent, so LLVM emits 2-wide `fsqrt.2d` / `fdiv.2d`),
  then the updates. A Rust program with exactly the `main.velt` shape (`Vec<Box<Body>>`, the same
  loops) is slower than `main.velt`. The optimized inner pair loop has no bounds checks left.
- **`main_opt.velt` (1.85×): arrays built with `push` in a loop stay in memory.** It uses the
  Rust #3 shape with `dPos: Vec3[]` and `mags: f64[]` created by `push`ing 10 elements in a
  loop, so the buffer pointer LLVM sees is a loop phi of `velt_rt_alloc` / `velt_rt_realloc`
  results, which its alias analysis does not see through: after LLVM unrolls the step loop, every
  step loads and stores the arrays' elements. Built as literals or with 10 straight-line `push`
  calls (one allocation), the step loop has the load and store counts of a Rust `Vec` version,
  8 of the 10 `sqrt` / `fdiv` vectorize, and it runs 13–27% faster. Building the arrays with
  `new Array<T>(n).fill(v)` would give `main_opt.velt` the same
  ([../../FINDINGS.md](../../FINDINGS.md) §2). That version is still about 1.3× the Rust `Vec`
  version with the same optimized IR; clang 22 and clang 18 make no difference on x86_64.
- **Cranelift** takes about 2× the CPU of LLVM: no auto-vectorization and no loop-invariant
  code motion.
