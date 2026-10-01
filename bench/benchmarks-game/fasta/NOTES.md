# fasta

> The timing tables below were measured while other agents shared the machine. The authoritative
> numbers are the serial run in `../RESULTS.md`.

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
| `main.vlt` | idiomatic Velt: `Random`, `Repeat`, `Alphabet` classes, `u8[]` blocks, `openWrite("/dev/stdout")` | | 1 |
| `main_mt.vlt` | `main.vlt` + main draws each block's random numbers, a spawned task picks the letters; one block per core per round, `Promise.all` keeps the order | | all cores |

Deviations: Rust #7 uses `spin::Mutex` and `num_cpus`; the bench project only depends on rayon and
regex, so it uses `std::sync::Mutex` and `std::thread::available_parallelism()` (the busy-retry
loops are unchanged). On aarch64 its SSE2 path is compiled out and the scalar 16-way count is used.
Everything else is unchanged.

Correctness: every implementation matches `fasta-output.txt` at 1000 and the Rust output at
25,000,000 byte for byte (Velt: both backends, both variants). Node #5 writes a reused buffer
with `process.stdout.write`, which is asynchronous on macOS pipes: piped into another process
(`node main.js 25000000 | cmp …`) its output is corrupted; redirected to a file (as `run.sh`
does) it is correct.

## Timings (Apple M4, 10 cores; best of 2 sessions × 5 runs; stdout to a file)

The machine was shared with other agents during both sessions (load average 12–35), so wall
times are inflated and noisy; CPU seconds are the more reliable column. Versions: velt 0.1.0 (8f35554), rustc 1.98.1 (`-C target-cpu=native`, LTO), go 1.27.1, node 24.11.1, bun 1.4.2.

| Implementation | wall s | CPU s | RSS MB |
|---|---:|---:|---:|
| Rust fasta (#7, 2 threads) | 1.337 | 1.794 | 2.3 |
| Rust fasta_st (#3) | 3.101 | 2.890 | 1.5 |
| Velt LLVM main | 4.063 | 3.779 | 3.8 |
| Velt Cranelift main | 4.758 | 4.447 | 3.7 |
| Velt LLVM main_mt | 1.732 | 3.319 | 31.8 |
| Velt Cranelift main_mt | 2.041 | 4.461 | 34.4 |
| Go main (#2, parallel) | 1.709 | 3.359 | 13.3 |
| Go main_st (#1) | 4.826 | 4.585 | 3.8 |
| Node main (#5, 4 workers) | 1.774 | 4.886 | 104.6 |
| Bun main (same source) | 1.645 | 4.357 | 66.6 |

(An earlier, quieter run: Rust #7 0.995 s wall, Rust #3 2.25 s wall / 2.19 s CPU.)

## Gap: Velt main vs Rust #3 (single thread), ~1.3–1.7× CPU

Three separate causes, measured with variants of `main.vlt` (hyperfine, 5 runs, CPU):

| Variant | CPU s |
|---|---:|
| `main.vlt` as is | 4.24 |
| + LCG seed in a local instead of `rng.seed` | 3.66 |
| + integer thresholds instead of `seed / IM` compared with f64 cumulative probabilities | 2.49 |
| Rust #3 | 2.23 |

1. **Algorithm (not a compiler issue).** `main.vlt` does what JS and Go #1 do: `seed / IM` as a
   float and a linear search over f64 cumulative probabilities. Rust #3 precomputes
   `floor(p * IM)` as integers and compares the seed directly. Go #1 (same float algorithm) is
   slower than Velt. With Rust #3's integer thresholds Velt is within 1.1× of it.

2. **Class-typed parameters get no `noalias` (lowering).** `Alphabet.next(rng: Random, …)`
   modifies `rng`, yet its pointer has no attributes, while `this` gets them:

   ```llvm
   define internal void @"_V8AlphabetM4next"(ptr readonly nonnull dereferenceable(48) %p0,
       ptr %p1, i64 %p2, ptr noalias nonnull dereferenceable(24) %p3)
   ```

   so `rng.seed` is loaded and stored on every number, and the store→load round trip sits on the
   LCG's loop-carried dependency (`ldr x8,[x21] … str x10,[x21]` in the hot loop). Root cause:
   `crates/velt_vir/src/lower/abi.rs` classifies only `this` as `PtrParam::Object`; any other
   class-typed param falls into `PtrParam::NotPtr` and gets `ParamAttrs::default()`.
   **Fix:** classify every class-typed param (a pointer to the object) as `PtrParam::Object`, so
   `param_attrs` gives `BorrowMut` ones `noalias` and `Borrow` ones `readonly` (the exclusivity
   rule already guarantees it). Worth ~15% here (4.24 → 3.66 s).

3. **Writes wait on the blocking pool.** `await out.writeBytes(block)` hands each 61 KB block to
   the runtime's blocking pool and suspends until it is written; wall time exceeds CPU time by
   ~0.3–0.8 s (e.g. 4.06 s wall vs 3.78 s CPU; under load up to 6.9 s wall). Rust writes
   synchronously from the same thread. **Fix (runtime):** let `FileWriter.write*` complete
   synchronously when the data fits in the writer's buffer (only flushes need the pool), or
   give std a synchronous `process.stdout.write(bytes)`.

Cranelift is 15–20% behind LLVM on the same code.

The multi-threaded Velt version is close to Go #2 in wall and CPU time; Rust #7 (2 threads)
remains ahead (1.34 s vs 1.73 s wall).

## Friction for a TypeScript developer
- `for (let i = 0; i < text.length; i++)` is a type error: `i` is `i64`, `length` is `usize`
  (no implicit numeric conversions); every index loop needs `let i: usize = 0` and `as` casts
  (`charCodeAt(i as i64)`).
- No `new Uint8Array(n)` / `new Array(n).fill(0)`: byte buffers can only grow by `push`
  (`__intrinsic_array_with_capacity` is std-only).
- No synchronous `process.stdout.write`; binary output needs `openWrite("/dev/stdout")` and an
  `async main`, and async parameters are owned (a `Random` passed to an async helper is moved),
  which is why the section loops stay in `main`.
- `Math.min` is `f64`-only, so `left < BLOCK_SIZE ? left : BLOCK_SIZE` for integers.

## Std and runtime round
Output goes through `stdout.write` (std/process), which is synchronous and portable, instead of
`await openWrite("/dev/stdout")`, which waited on the blocking pool for every block.
