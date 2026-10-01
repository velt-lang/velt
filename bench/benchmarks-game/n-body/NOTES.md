# n-body

> The timing tables below were measured while other agents shared the machine. The authoritative
> numbers are the serial run in `../RESULTS.md`.

Official description: https://benchmarksgame-team.pages.debian.net/benchmarksgame/description/nbody.html
(N = 50,000,000; QUICK_N = 1000, `expected-quick.txt` = the official `nbody-output.txt`).

## Sources and credits

| file | source |
|---|---|
| `rust/src/bin/n-body.rs` | Rust #3, Ilia Schelokov: the fastest published Rust without `std::arch::x86_64` intrinsics (#9, #7, #5, #6 are x86-only and do not build on the M4) |
| `main.go` | Go #3, The Go Authors / Christoph Bauer / Isaac Gouy / Antonio Petri (fastest Go) |
| `main.js` | Node.js #6, Isaac Gouy / Andrey Filatkin (fastest Node); Bun runs the same file |
| `main.vlt` | idiomatic Velt: a `Body` class, `Body[]`, the Node #6 loop shape |
| `main_opt.vlt` | the Rust #3 loop shape with Copy structs (`Vec3`, `Body`) stored inline, see "Gaps" |

All published n-body programs are single-threaded, so there is no `_mt` variant. The reference
files are unchanged apart from a first `// Source:` line.

## Deviations
- Velt has no `Number.prototype.toFixed`; each Velt file declares it with `extend f64 { toFixed }`
  (rounds `|x| * 10^9` to an integer: exact for the magnitudes printed here).
- `main.vlt` computes `dx * (mass * mag)` (the Rust #3 order) where Node #6 has
  `dx * mass * mag`; the printed energies are identical at QUICK_N and N.

## Correctness
Every implementation (Velt LLVM and Cranelift, both files; Rust; Go; Node; Bun) prints the official
output at N = 1000 and the Rust output at N = 50,000,000 (`-0.169075164` / `-0.169059907`).

## Timings (Apple M4, 10 cores: 4P + 6E, 32 GB, macOS)

rustc 1.98.1 (`-C target-cpu=native`, LTO, 1 CGU), velt 0.1.0 (8f35554, `--release`; LLVM =
Apple clang 21), Go 1.27.1, Node 24.11.1, Bun 1.4.2. Best of two rounds of 3 runs. The machine was
shared with other agents' builds and benchmarks (load average 10–33), so wall times vary by
±15–30 % between rounds; CPU time is the steadier number.

| implementation | threads | wall s | CPU s | peak RSS MB | wall × Rust | CPU × Rust |
|---|---|---:|---:|---:|---:|---:|
| Rust `n-body.rs` | 1 | 1.66 | 1.64 | 1.4 | 1.00 | 1.00 |
| Velt LLVM `main.vlt` | 1 | 2.46 | 2.44 | 2.0 | 1.48 | 1.49 |
| Velt LLVM `main_opt.vlt` | 1 | 2.89 | 2.87 | 2.1 | 1.74 | 1.75 |
| Velt Cranelift `main.vlt` | 1 | 5.77 | 5.59 | 2.0 | 3.48 | 3.41 |
| Velt Cranelift `main_opt.vlt` | 1 | 6.65 | 6.50 | 2.1 | 4.01 | 3.96 |
| Go `main.go` | 1 | 2.60 | 2.56 | 3.9 | 1.57 | 1.56 |
| Node `main.js` | 1 | 3.18 | 3.17 | 50.5 | 1.92 | 1.93 |
| Bun `main.js` | 1 | 4.03 | 4.00 | 25.0 | 2.43 | 2.44 |

## Gaps (Velt LLVM > 1.2× Rust)

### `main.vlt` 1.49×: the loop shape, not the compiler
Node #6 (and `main.vlt`) handles one pair at a time: distance → `sqrt` → divide → velocity
update, one scalar `sqrt` + `fdiv` per pair on the critical path. Rust #3 first computes all 10
distance vectors, then all 10 magnitudes (independent, so LLVM emits 2-wide `fsqrt.2d` /
`fdiv.2d`), then the updates. To separate the two, a scratch Rust program with exactly the
`main.vlt` shape (`Vec<Box<Body>>`, the same loops) took 4.2–4.4 s CPU against 3.4–3.5 s for
`main.vlt` in the same session: for this shape Velt is faster than Rust. In the optimized IR the
inner pair loop keeps 9 loads and 2 stores per pair (the `bodies[j]` fields); there are no bounds
checks left.

No compiler fix needed; `main_opt.vlt` shows what the Rust #3 shape gets.

### `main_opt.vlt` 1.75×: arrays built with `push` in a loop stay in memory
`main_opt.vlt` uses the Rust #3 shape with `dPos: Vec3[]` and `mags: f64[]` created by
`push`ing 10 elements in a loop. After LLVM fully unrolls the step loop, every step does
**169 loads and 80 stores** and 10 scalar `sqrt` + 10 `fdiv` (counted in the optimized IR of the
step loop). Rust #3 (stack arrays) keeps everything in registers: 0 loads, 0 stores, most
`sqrt`/`fdiv` 2-wide; a scratch Rust version with `Vec`s instead of arrays has 33 loads / 17 stores.

The VIR builds each array through the prelude's growth path, so the buffer pointer LLVM sees is a
loop phi of `velt_rt_alloc` / `velt_rt_realloc` results:

```
    _186 = call extern#8 velt_rt_alloc(96_u64, 8_u64) -> bb107         // first push
    _186 = call extern#20 velt_rt_realloc(_254, _188, 8_u64, _189) -> bb107  // growth
    ...
    _88 = ptradd _254, _87                                              // dPos[k] = ...
    (*_88 as agg#2) = agg#2 { _199, _200, _201 }
```

Experiments (scratch copies of `main_opt.vlt`):
- the same arrays written as literals (`[Vec3 {…}, … ×10]`, one `velt_rt_alloc`) or as 10
  straight-line `push` calls: the step loop drops to **33 loads / 17 stores**, 8 of the 10 `sqrt`/`fdiv` vectorized (two `<4 x double>` ops each),
  the same load/store counts as the Rust `Vec` version, and 13–27 % less time (two sessions);
- that version is still 1.3× the Rust `Vec` version (2.76–2.83 s against 2.08–2.15 s CPU,
  5 alternating runs), although the optimized IR of both step loops has the same instruction mix
  (84 `fmul`, 40 `fsub`, 38 `fadd`, 33 loads, 17 stores). The difference is in the machine code:
  Rust's LLVM 22 emits 65 `fmul.2d` + 5 scalar `fmul` and 10 `str`, Apple clang 21 (what Velt
  uses) 43 `fmul.2d` + 41 scalar `fmul` and 31 `str`/`stur`. Not investigated further.

Proposed fixes:
1. std/prelude (verified effective above): TS-style preallocation that lowers to one
   `__intrinsic_array_with_capacity` allocation: `new Array<T>(n).fill(v)` and
   `Array.from({ length: n }, (_, i) => …)`. Today neither exists (`cannot find class
   `Array``), so a TypeScript developer can only `push` in a loop.
2. velt_opt / LLVM backend (to try): tell LLVM that the result of a push loop is still a fresh,
   unaliased allocation, e.g. declare `velt_rt_alloc` / `velt_rt_realloc` / `velt_rt_free` with
   `allockind(...)`, `allocsize(0)` and an `"alloc-family"` like rustc does for `__rust_alloc`, or
   hoist the capacity check of a counted `push` loop (reserve `n` once before the loop).
3. Build with a current LLVM (`VELT_CLANG` pointing at clang 22) and re-measure the remaining 1.3×.

### Cranelift 3.4–4×
The known Cranelift limits (bench/RESULTS.md): no auto-vectorization and no loop-invariant code
motion; `main_opt.vlt`'s 10-wide unrolled step loop does not get unrolled or vectorized at all.

## Friction for a TypeScript developer (all hit while writing `main.vlt`)
- `toFixed` is missing (worked around with `extend f64`, which is pleasant).
- Constructor parameter properties (`constructor(public x: f64, …) {}`) are a parse error
  (``expected `:`, found `x` ``): the class needs 7 field declarations plus 7 assignments.
- `const bi = bodies[i]` on a class element is ``cannot move out of an array element; use
  .clone() or pop()``, so the JS idiom of aliasing an element does not work: every access is
  written `bodies[i].x`.
- `for (const body of bodies) { body.x += … }` is ``cannot assign to a field of `body` ``
  (for...of borrows): index the array instead.
- `for (let i = 0; i < bodies.length; i++)` is `mismatched types` (`i` is `i64`, `length` is
  `usize`): the loop variable needs `let i: usize = 0`.
- Reading N: `argv()` from `std/process` and `Number(args[1]) as i64` (no global `process.argv`).

## Pending goldens
No wrong output or crash was found. Gaps filed in `tests/golden/bugs/`:
`parse_constructor_parameter_properties` (TS parameter properties) and `std_array_new_fill`
(`new Array<T>(n).fill(v)` / `Array.from`); the missing `toFixed` is already filed as
`std_number_to_fixed`.
