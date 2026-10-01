# fannkuch-redux

> The timing tables below were measured while other agents shared the machine. The authoritative
> numbers are the serial run in `../RESULTS.md`.

Official description: https://benchmarksgame-team.pages.debian.net/benchmarksgame/description/fannkuchredux.html
(N = 12; QUICK_N = 7, `expected-quick.txt` = the official `fannkuchredux-output.txt`).

## Sources and credits

| file | source |
|---|---|
| `rust/src/bin/fannkuch-redux.rs` | Rust #4 (Rust Project Developers, TeXitoi, Cristi Cobzarenco): rayon over 24 blocks of permutation indices. The faster Rust #6 uses x86 SSSE3 shuffles and does not build on the M4 |
| `rust/src/bin/fannkuch-redux_st.rs` | the same program with rayon's global pool set to 1 thread (one added line in `main`): no single-threaded Rust is published |
| `main.go` | Go #3 (Oleg Mazurov, Isaac Gouy, Jan Pfeifer): fastest Go, `GOMAXPROCS(4)` hard-coded |
| `main_st.go` | Go #8 (Isaac Gouy, after Rex Kerr's Scala): fastest single-threaded Go |
| `main.js` | Node.js #5, Andrey Filatkin (after Oleg Mazurov's Go): worker threads |
| `main_st.js` | Node.js #8, Isaac Gouy (after Rex Kerr's Scala): fastest single-threaded Node |
| `main.vlt` | idiomatic single-threaded Velt: the classic sequential algorithm of Node #8 (`count[]` rotations, whole-prefix reversal) |
| `main_mt.vlt` | the Rust #4 algorithm: 24 blocks of permutation indices, each a spawned task that starts from its first permutation (factorial-base digits), `Promise.all` combines checksums and maxima; the flip loop is Rust #4's |

The official files are unchanged apart from a first `// Source:` line (`_st.rs`: three lines).
Naming as in spectral-norm: `main.*` fastest published, `main_st.*` fastest single-threaded,
Rust `_st` = the reference for single-threaded Velt.

## Deviations
None in the ports. `main_st.js` (official Node #8) assigns undeclared variables
(`f = 0, flips = 0, …`); Node runs it, **Bun 1.4.2 stops with `ReferenceError: f is not
defined`**, so the Bun `main_st.js` row is n/a.

## Correctness
Every implementation (except Bun `main_st.js`) prints the official output at N = 7 and the Rust
output at N = 12 (`3968050` / `Pfannkuchen(12) = 65`). Both Velt programs, both backends, also
match Rust for N = 1…10 (`main_mt.vlt` handles fewer permutations than blocks and N = 1).

## Timings (Apple M4, 10 cores: 4P + 6E, 32 GB, macOS)

Versions and conditions as in n-body/NOTES.md (best of two rounds of up to 3 runs; runs over 20 s
once per round; loaded, shared machine).

| implementation | threads | wall s | CPU s | peak RSS MB | wall × Rust | CPU × Rust |
|---|---|---:|---:|---:|---:|---:|
| Rust `fannkuch-redux.rs` | all | 6.01 | 25.81 | 1.8 | 1.00 | 1.00 |
| Rust `fannkuch-redux_st.rs` | 1 | 27.27 | 25.21 | 1.6 | 1.00 | 1.00 |
| Velt LLVM `main.vlt` | 1 | 38.48 | 36.56 | 2.0 | 1.41 | 1.45 |
| Velt LLVM `main_mt.vlt` | all | 6.15 | 29.62 | 3.6 | 1.02 | 1.15 |
| Velt Cranelift `main.vlt` | 1 | 70.51 | 68.00 | 2.0 | 2.59 | 2.70 |
| Velt Cranelift `main_mt.vlt` | all | 16.07 | 55.82 | 3.6 | 2.67 | 2.16 |
| Go `main.go` | 4 | 8.48 | 27.09 | 4.5 | 1.41 | 1.05 |
| Go `main_st.go` | 1 | 39.57 | 38.32 | 4.0 | 1.45 | 1.52 |
| Node `main.js` | all | 10.65 | 43.26 | 147.0 | 1.77 | 1.68 |
| Bun `main.js` | all | 8.99 | 32.39 | 86.9 | 1.50 | 1.25 |
| Node `main_st.js` | 1 | 45.82 | 43.56 | 48.1 | 1.68 | 1.73 |
| Bun `main_st.js` | 1 | n/a | | | | |

## Gaps (Velt LLVM > 1.2× Rust)

### `main.vlt` 1.45×: the algorithm, not the compiler
`main.vlt` copies every permutation and reverses the whole prefix per flip (the classic
program). Rust #4 skips permutations starting with 0 and, per flip, stores the old first value
at its final place and reverses only the elements in between. A scratch Rust program with
exactly the `main.vlt` loops (`Vec<i64>`) runs N = 11 in 2.75–2.94 s CPU against 2.77–2.93 s for
`main.vlt` (three alternating runs): parity.

`main_mt.vlt` uses Rust #4's algorithm and is within 1.15× of Rust in CPU time (1.02× wall); on
one thread (`VELT_THREADS=1`, N = 11) 2.23 s against 1.97 s for `fannkuch-redux_st.rs`. What is
left is data layout: Rust works on `[i32; 16]` stack arrays, Velt on heap `i64[]` with bounds
checks; no further fix proposed at this size.

### Cranelift 2.2–2.7×
The known Cranelift limits (bench/RESULTS.md: no loop-invariant code motion, no unrolling or
vectorization); not analysed further for this program.

## Friction for a TypeScript developer
- A function returning two numbers: an object type `{ checksum: i64; maxFlips: i64 }` and an
  object literal work as in TypeScript; tuples would be the Rust habit.
- `Promise.all` over spawned tasks and `for (const block of await Promise.all(tasks))` read
  exactly like TypeScript.
- No `new Array(n).fill(0)`: the three work arrays are built with `push` loops.
- Upper bounds (`Math.min(start + blockSize, total)`) on integers need a ternary: `Math.min` is
  `f64`-only.

## Pending goldens
No wrong output or crash was found (`main_mt.vlt` had an out-of-bounds panic at N = 1 in my own
next-permutation code, fixed). Integer `Math.min` is filed as
`tests/golden/bugs/std_math_min_integers`, preallocation as `std_array_new_fill`.
