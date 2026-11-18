# fannkuch-redux: port notes

Official description: https://benchmarksgame-team.pages.debian.net/benchmarksgame/description/fannkuchredux.html
(N = 12; QUICK_N = 7, `expected-quick.txt` = the official `fannkuchredux-output.txt`).

## Sources and credits

| file | source |
|---|---|
| `rust/src/bin/fannkuch-redux.rs` | Rust #4 (Rust Project Developers, TeXitoi, Cristi Cobzarenco): rayon over 24 blocks of permutation indices. The faster Rust #6 uses x86 SSSE3 shuffles and does not build on arm64 |
| `rust/src/bin/fannkuch-redux_st.rs` | the same program with rayon's global pool set to 1 thread (one added line in `main`): no single-threaded Rust is published |
| `main.go` | Go #3 (Oleg Mazurov, Isaac Gouy, Jan Pfeifer): fastest Go, `GOMAXPROCS(4)` hard-coded |
| `main_st.go` | Go #8 (Isaac Gouy, after Rex Kerr's Scala): fastest single-threaded Go |
| `main.js` | Node.js #5, Andrey Filatkin (after Oleg Mazurov's Go): worker threads |
| `main_st.js` | Node.js #8, Isaac Gouy (after Rex Kerr's Scala): fastest single-threaded Node |
| `main.velt` | idiomatic single-threaded Velt: the classic sequential algorithm of Node #8 (`count[]` rotations, whole-prefix reversal) |
| `main_mt.velt` | the Rust #4 algorithm: 24 blocks of permutation indices, each a spawned task that starts from its first permutation (factorial-base digits), `Promise.all` combines checksums and maxima; the flip loop is Rust #4's |

The official files are unchanged apart from a first `// Source:` line (`_st.rs`: three lines).

## Deviations

None in the ports. `main_st.js` (official Node #8) assigns undeclared variables
(`f = 0, flips = 0, …`); Node runs it, Bun 1.4.2 stops with `ReferenceError: f is not defined`,
so the Bun `main_st.js` row is n/a.

## Correctness

Every implementation (except Bun `main_st.js`) prints the official output at N = 7 and the Rust
output at N = 12 (`3968050` / `Pfannkuchen(12) = 65`). Both Velt programs, both backends, also
match Rust for N = 1…10 (`main_mt.velt` handles fewer permutations than blocks, and N = 1).

## Where the time goes

Timings are in [../RESULTS.md](../RESULTS.md).

- **`main.velt` (1.44× Rust CPU) is the algorithm, not the compiler.** It copies every
  permutation and reverses the whole prefix per flip (the classic program). Rust #4 skips
  permutations starting with 0 and, per flip, stores the old first value at its final place and
  reverses only the elements in between. A Rust program with exactly the `main.velt` loops
  (`Vec<i64>`) runs at parity with `main.velt`.
- **`main_mt.velt`** uses Rust #4's algorithm and is within 1.06× of Rust in CPU time (1.10×
  wall). What is left is data layout: Rust works on `[i32; 16]` stack arrays, Velt on heap
  `i64[]` with bounds checks.
- **Cranelift** is 1.8–2× slower than LLVM: no loop-invariant code motion, unrolling or
  vectorization.
