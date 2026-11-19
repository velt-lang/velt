# k-nucleotide: port notes

Rules: <https://benchmarksgame-team.pages.debian.net/benchmarksgame/description/knucleotide.html>.
Stdin is the output of `fasta 25000000` (official size); quick stdin is `fasta 1000` = the
official 10 KB `knucleotide-input.txt`, and `expected-quick.txt` is the official
`knucleotide-output.txt`.

## Sources

| File | Program | Authors | Threads |
|---|---|---|---|
| `rust/src/bin/k-nucleotide.rs` | [Rust #7](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/knucleotide-rust-7.html) (fastest) | Alexander | 7 |
| `rust/src/bin/k-nucleotide_st.rs` | Rust #7 with the frame lengths counted one after another | | 1 |
| `main.go` | [Go #7](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/knucleotide-go-7.html) | Mark van Weert (based on Go #6 / C++ #2) | all cores |
| `main.js` | [Node #3](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/knucleotide-node-3.html) | Dani Biro (based on Node #2 by Jesse Millikan, Matt Baker, Roman Pletnev) | 4 workers |
| `main.velt` | idiomatic Velt: `readLineSync` line by line, `Map<i64, i64>` counts keyed by the 2-bit packed k-nucleotide, `upsert` | | 1 |
| `main_mt.velt` | `main.velt` with one spawned task per length (like Rust #7) reading one `shared` sequence, `Promise.all` | | 7 tasks |

Deviations:
- Rust #7 depends on hashbrown, futures, tokio_threadpool, itertools and num. The bench project
  has none of them (`rust/Cargo.toml`), so: `std::collections::HashMap` with an inline FxHash hasher
  (hashbrown's default aHash / foldhash is likewise a fast non-cryptographic hash; std's SipHash
  would be several times slower), `std::thread::scope` threads instead of the thread pool (one
  per length, as before), `Vec::sort_by` instead of `itertools::sorted_by`, and `From<u8>` +
  `T::mask` instead of `num::FromPrimitive`. The keys stay `u8` / `u16` / `u32` / `u64` per length.
- Node #3 has an off-by-`length` bug: its counting loop stops `length` frames before the end,
  so the official 10 KB input gives `T 31.500` instead of `31.520`. Fixed in one line
  (`const n = seq.length + 1;`, commented in the source); the 25M output was already right.

Correctness: every implementation matches the official output at 1000 and the Rust output at
25,000,000 (Velt: both backends, both variants).

## Where the time goes

Timings are in [../RESULTS.md](../RESULTS.md). `main.velt` takes about 2.3× the CPU of the
single-threaded Rust; most of it is in `frequencies`, where the `Map` code is inlined.

- **`Map` against hashbrown.** Counting one length, `Map<i64, i64>.upsert` costs about what
  hashbrown with FxHash does for a handful of keys and 2–3× as much from 16 keys up. A hit
  probes 8-byte slot words (hashbrown: a 1-byte control group), then reads the key from the
  dense, insertion-ordered key array and updates a `V | null` value; the insertion order is what
  JS iteration requires. The lookup returns the entry position on a hit, so `upsert` reads the
  slot once. Interleaving keys and values in one entry array and dropping the Fibonacci
  multiply were measured and are slower ([../../RESULTS.md](../../RESULTS.md) "Lessons from
  optimization work"). The real Rust program also uses `u8` / `u16` keys for the short lengths.
- **Reading lines.** `readLineSync` reads through one process-wide read-ahead buffer
  (`crates/velt_rt/src/stdin.rs`): a buffered line is one memchr and one copy. `charCodeAt` is
  inline code (a bounds check and a byte load).
- **Multi-threaded.** `main_mt.velt` gives its tasks one `shared(seq)`: a `shared<T>` that holds
  no `Mutex` is read like a `T`, so the 125 MB sequence is not copied per task. The `main_mt`
  rows in RESULTS.md (about 1 GB peak RSS) were measured when each task got its own copy.
