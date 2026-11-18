# fasta: port notes

Rules: <https://benchmarksgame-team.pages.debian.net/benchmarksgame/description/fasta.html>.
Official size `25000000` (254,166,745 bytes of output); quick size `1000` = the official
`fasta-output.txt` (`expected-quick.txt`).

## Sources

| File | Program | Authors | Threads |
|---|---|---|---|
| `rust/src/bin/fasta.rs` | [Rust #7](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/fasta-rust-7.html) (fastest) | the Rust Project Developers, TeXitoi, Alisdair Owens, Ryohei Machida | 2 |
| `rust/src/bin/fasta_st.rs` | [Rust #3](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/fasta-rust-3.html) (fastest single-threaded) | the Rust Project Developers, TeXitoi, Matt Brubeck | 1 |
| `main.go` | [Go #2](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/fasta-go-2.html) | The Go Authors, Joern Inge Vestgaarden, Jorge Peixoto de Morais Neto, Isaac Gouy, INADA Naoki | all cores |
| `main_st.go` | [Go #1](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/fasta-go-1.html) | The Go Authors et al. | 1 |
| `main.js` | [Node #5](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/fasta-node-5.html) | Roman Pletnev, Andrey Filatkin (based on Petr Prokhorenkov / Jos Hirth et al.) | 4 workers |
| `main.velt` | idiomatic Velt: `Random`, `Repeat`, `Alphabet` classes, `u8[]` blocks written with `stdout.write` | | 1 |
| `main_mt.velt` | `main.velt` + main draws each block's random numbers, a spawned task picks the letters; one block per core per round, `Promise.all` keeps the order | | all cores |

Deviations: Rust #7 uses `spin::Mutex` and `num_cpus`, which the bench project does not depend on
(`rust/Cargo.toml`), so it uses `std::sync::Mutex` and `std::thread::available_parallelism()`
(the busy-retry loops are unchanged). On aarch64 its SSE2 path is compiled out and the scalar
16-way count is used. Everything else is unchanged.

Correctness: every implementation matches `fasta-output.txt` at 1000 and the Rust output at
25,000,000 byte for byte (Velt: both backends, both variants). Node #5 writes a reused buffer
with `process.stdout.write`, which is asynchronous on macOS pipes: piped into another process
(`node main.js 25000000 | cmp …`) its output is corrupted; redirected to a file (as `run.sh`
does) it is correct.

## Where the time goes

Timings are in [../RESULTS.md](../RESULTS.md). `main.velt` takes about 1.2× the CPU of Rust #3
(single-threaded).

- **Algorithm.** `main.velt` does what JS and Go #1 do: `seed / IM` as a float and a linear
  search over `f64` cumulative probabilities. Rust #3 precomputes `floor(p * IM)` as integers and
  compares the seed directly. With Rust #3's integer thresholds Velt is within 1.1× of it; Go #1
  (the same float algorithm) is slower than Velt.
- **The generator's state.** `Alphabet.next(rng: Random, …)` modifies `rng`; a class-typed
  parameter the function modifies is `noalias` (`crates/velt_vir/src/lower/param_attrs.rs`), so
  LLVM keeps `rng.seed` in a register for the whole block instead of storing and reloading it on
  the LCG's loop-carried dependency.
- **Output.** `stdout.write` (`velt/process`) is synchronous and ordered with `console.log`; from
  64 KiB up it writes straight from the caller's array.
- **Cranelift** is 4% behind LLVM on `main` and uses 50% more CPU on `main_mt`.
- `main_mt.velt` is close to Go #2 in wall and CPU time; Rust #7 (2 threads) remains ahead.
