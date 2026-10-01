# k-nucleotide

> The timing tables below were measured while other agents shared the machine. The authoritative
> numbers are the serial run in `../RESULTS.md`.

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
| `main.vlt` | idiomatic Velt: `readLineSync` line by line, `Map<i64, i64>` counts keyed by the 2-bit packed k-nucleotide, `upsert` | | 1 |
| `main_mt.vlt` | `main.vlt` with one spawned task per length (like Rust #7), `Promise.all` | | 7 tasks |

Deviations:
- Rust #7 depends on hashbrown, futures, tokio_threadpool, itertools and num. The bench project
  only has rayon and regex, so: `std::collections::HashMap` with an inline FxHash hasher
  (hashbrown's default aHash / foldhash is likewise a fast non-cryptographic hash; std's SipHash
  would be several times slower), `std::thread::scope` threads instead of the thread pool (one
  per length, as before), `Vec::sort_by` instead of `itertools::sorted_by`, and `From<u8>` +
  `T::mask` instead of `num::FromPrimitive`. The keys stay `u8` / `u16` / `u32` / `u64` per length.
- Node #3 has an off-by-`length` bug: its counting loop stops `length` frames before the end,
  so the official 10 KB input gives `T 31.500` instead of `31.520`. Fixed in one line
  (`const n = seq.length + 1;`, commented in the source); the 25M output was already right.

Correctness: every implementation matches the official output at 1000 and the Rust output at
25,000,000 (Velt: both backends, both variants).

## Timings (Apple M4, 10 cores; best of 2 sessions × 3 runs; stdout to a file)

The machine was shared with other agents during both sessions (load average 12–35), so wall
times are inflated and noisy; CPU seconds are the more reliable column. Versions: velt 0.1.0 (8f35554), rustc 1.98.1 (`-C target-cpu=native`, LTO), go 1.27.1, node 24.11.1, bun 1.4.2.

| Implementation | wall s | CPU s | RSS MB |
|---|---:|---:|---:|
| Rust k-nucleotide (#7, 7 threads) | 1.547 | 2.968 | 132.9 |
| Rust k-nucleotide_st | 4.017 | 3.163 | 130.3 |
| Velt LLVM main | 9.883 | 7.787 | 246.0 |
| Velt Cranelift main | 10.471 | 10.278 | 246.1 |
| Velt LLVM main_mt | 3.550 | 7.738 | 1008.0 |
| Velt Cranelift main_mt | 5.415 | 12.940 | 1014.3 |
| Go main (#7, parallel) | 4.697 | 18.152 | 242.1 |
| Node main (#3, 4 workers) | 14.656 | 37.647 | 431.0 |
| Bun main (same source) | 9.833 | 24.309 | 401.9 |

## Gap: Velt main vs Rust single-threaded, ~2.5× CPU

A `sample` profile of `main.vlt` (LLVM): ~75% in `frequencies` (the `Map` code is inlined into
it), ~15% reading stdin.

### 1. `Map` upsert costs 2–2.5× hashbrown's for maps of 16+ keys (std prelude)

Counting one length over the 125M-base sequence, `Map<i64, i64>.upsert` vs Rust
`HashMap<i64, i64, Fx>` `entry().or_insert() += 1` (best of 3, loaded machine):

| length (distinct keys) | Rust ms | Velt ms |
|---|---:|---:|
| 1 (4) | 280 | 297 |
| 2 (16) | 275 | 678 |
| 12 (138,127) | 516 | 1561 |
| 18 (139,882) | 635 | 1408 |

(The real Rust program also uses `u8`/`u16` keys for the short lengths, which makes its small
maps cheaper still.) The hot path after inlining (`--emit vir`, `frequencies`):

```
    _24 = mul _23, 5871781006564002453_u64        // FxHash
    _25 = mul _24, 11400714819323198485_u64       // * 2^64/phi (Map's scrambling)
    _26 = call fn#6 _V3stdP7preludeP3mapN3MapM4find_T6_6(_4, _2, _25)
    ...                                           // hit:
    _37 = (*_4 as agg#18).4.1                     // slots.length   (bounds check)
    ...                                           // reload slots[s] → position
    _33 = (*_4 as agg#18).2.1                     // entryValues.length (bounds check)
    _32 = ptradd (*_4 as agg#18).2.0, _31
    _294 = (*_32 as agg#15).0                     // `V | null` tag of the value
```

A hit touches three arrays (`slots` → `entryKeys[pos]` in `find`, then `slots[s]` again and
`entryValues[pos]` in `upsert`), each with a bounds check, and the value is a 16-byte `V | null`.
hashbrown touches a control group and one bucket holding key and value. The design (dense
insertion-ordered entries + Robin Hood index) is needed for JS iteration order, but the hit path
can be made cheaper. **Proposed (std/prelude/map.vlt):**
- `find` returns the entry position on a hit (it already has it: `(w & POS_MASK) - 1`), so
  `upsert` / `get` / `update` don't re-read `slots[s]` (one load and one bounds check less);
- store entries as one array of `{ key, value }` (AoS) so a hit's key compare and value update
  share a cache line;
- mark tombstones in the slot word / a key-side bit instead of `V | null`, so values are plain
  `V` (no tag load/store, 8 bytes instead of 16 for `i64`);
- a std-only unchecked index intrinsic for the invariant-guarded accesses (`positionAt`,
  `entryKeys[...]`).
- the extra Fibonacci multiply on top of FxHash is on the critical path of every lookup; for
  integer keys the Fx multiply already spreads the bits (hashbrown uses its top 7 bits directly).

(An attempt to measure the first item with a patched std under `VELT_STD` gave no usable signal
on the loaded machine; the list above is from the IR, not a measured win.)

### 2. Reading lines: an allocation-heavy `readLine` and an external call per `charCodeAt`
`readLineSync()` alone over the 254 MB input (4.2M lines) takes ~0.53 s CPU (~125 ns per line);
adding `line.charCodeAt(i)` per byte adds ~0.4 s. Causes:
- `velt_rt_stdin_read_line` (`crates/velt_rt/src/stdin.rs`) locks stdin per line, reads into a
  fresh `Vec`, then `String::from_utf8_lossy(&buf).into_owned()` copies it again (the `Cow` is
  borrowed for valid UTF-8, so `into_owned` is a second allocation + copy), then wraps it.
  **Fix:** `String::from_utf8(buf)` and fall back to lossy only on error (no copy), and hold the
  stdin lock across a `readLine` loop (or buffer lines in the runtime).
- `charCodeAt` is `declare function velt_rt_str_char_code_at(...)`, an opaque call per byte:
  ```llvm
  %t81 = call i64 @"velt_rt_str_char_code_at"(ptr %t79, i64 %t80)
  ```
  **Fix:** make it an intrinsic lowered inline (`i < len ? ptr[i] : -1`), like `StrLen`.

### 3. Multi-threaded: no read-only sharing across tasks (language / runtime)
`main_mt.vlt` must pass `seq.clone()` to each of the 7 tasks (async parameters are owned), so it
copies 7 × 125 MB and peaks at 1 GB RSS (Rust #7: 133 MB with an `Arc<Vec<u8>>`). `shared(seq)`
does not help: `shared<u8[]>` can't be indexed or read (`no field length on type shared<u8[]>`,
`cannot index a value of type shared<u8[]>`). **Proposed:** let `shared<T>` be read like a `T`
borrow (index, `length`, `for…of`, passing to a borrowing param), which is sound because a
`shared` value is immutable unless it holds a `Mutex`.

## Friction for a TypeScript developer
- No `Number.prototype.toFixed` at the time (filed as `std_number_to_fixed.vlt`): `main.vlt`
  uses a small `toFixed3` helper. (`Array.prototype.sort(compareFn)` has since shipped; the
  former `sortBy(cmp)` is gone.)
- Index loops need `usize` counters and casts (`2 * (length - 1 - i) as i64`).
- `while ((line = readLineSync()) != null)` doesn't compile: an assignment expression has type
  `void` (filed as `tests/golden/bugs/sema_assignment_expression_value.vlt`);
  `for (;;) { const line = readLineSync(); if (line == null) break; … }` works.
