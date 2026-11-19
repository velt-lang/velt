# Benchmarks

Programs and harnesses that measure Velt's generated code, standard library, runtime and
compiler, most of them against Rust, Node and Bun. [RESULTS.md](RESULTS.md) holds the latest
results of the suites at this level and the lessons from past optimization work;
[FINDINGS.md](FINDINGS.md) lists the open performance gaps.

## Top level

| path | what |
|---|---|
| `*.velt`, `rust/`, `node/` | the micro suite: `bench/run.sh [runs]` (Windows: `pwsh bench/run.ps1 -Runs N`) builds each program with Cranelift (debug and release) and LLVM, the Rust and Node versions, checks they print the same, and prints the table |
| `compare.ps1` | two compilers on the micro suite (or another `-Dir`), interleaved: `pwsh bench/compare.ps1 -Baseline <velt.exe> [-Runs N]` |
| `nightly.sh` | the nightly guard: instructions of each benchmark (cachegrind) against a baseline: `bench/nightly.sh [--baseline FILE] [--out FILE] [--threshold PERCENT] [--velt PATH]` |
| `compare_refs.sh` | `nightly.sh` for two git refs: `bench/compare_refs.sh BASE_REF HEAD_REF WORK_DIR [RUNS]` |
| `compiler_ab.sh`, `runtime_ab.sh` | instructions of every benchmark built by two commits' compilers, or linked against two runtimes |
| `program_instr.sh`, `two_compilers_instr.sh` | instructions of chosen programs built by two refs, or by two `velt` binaries |
| `vir_diff.sh`, `vir_identical.sh` | whether two compilers emit the same VIR for the benchmarks |
| `check_instr.sh`, `module_state_instr.sh`, `counted_field_args.sh` | instruction A/Bs of `velt check` and of specific program shapes |
| `rc_stats.ps1` | reference-count operations of each benchmark (`VELT_RC_STATS=1`, debug runtime) |
| `debug_alloc_runs.sh`, `debug_goldens_time.sh` | the cost of the checking allocator (`VELT_RT_DEBUG_ALLOC=1`) |
| `symbols.sh`, `extended_layouts_mem.sh` | single-question measurements (symbols; memory of extended layouts) |

## Suites

| folder | what | docs |
|---|---|---|
| `benchmarks-game/` | the Computer Language Benchmarks Game against Rust, Go, Node and Bun | [README](benchmarks-game/README.md), [RESULTS](benchmarks-game/RESULTS.md) |
| `typical/` | TypeScript-style workloads, each also run by Node, with Rust ports | [RESULTS](RESULTS.md#typical-typescript-workloads-benchtypical) |
| `async/`, `iter/`, `timers/` | async functions, generators and channels, timer precision | [RESULTS](RESULTS.md#async-benchasync) |
| `json/` | JSON parse and stringify, and the canada / citm / twitter corpora | [RESULTS](RESULTS.md#json-benchjson) |
| `stdport/` | standard-library components against the Rust crate for the same job | [README](stdport/README.md) |
| `structs/` | the `struct` value type against classes, flat arrays and Rust | [README](structs/README.md) |
| `strings_utf16/` | non-ASCII string workloads against Node | [README](strings_utf16/README.md) |
| `shared/` | `shared<T>` reads, `RwLock` against `Mutex` | [README](shared/README.md) |
| `jsx/` | the TSX fortunes page: hand-written, precompiled and generic | [README](jsx/README.md) |
| `web/` | the six TechEmpower tests with Postgres | [README](web/README.md), [RESULTS](web/RESULTS.md) |
| `http/` | HTTP server scaling, middleware, TLS, and the fetch client | [RESULTS](http/RESULTS.md) |
| `websocket/` | a WebSocket echo and broadcast server against Node, Bun and Rust | [README](websocket/README.md) |
| `chat/` | a chat app (auth, JSON, SQLite, rooms) in Velt, Go, Bun and Node | [README](chat/README.md), [SPEC](chat/SPEC.md) |
| `db/`, `sqlite/`, `feed/` | database clients against Node and Rust; SQLite rows; the feed API | [db README](db/README.md), [feed README](feed/README.md) |
| `reload/` | `velt dev`: save to first response | [RESULTS](reload/RESULTS.md) |
| `perf_lint/` | the programs behind `velt check --perf`'s rules | [README](perf_lint/README.md) |
| `showcase/` | one run of every comparison on one machine, for the website | [README](showcase/README.md) |
| `compile/`, `spread_order/`, `test_runner/`, `testspeed/` | compile time and memory, `velt test` throughput | [RESULTS](RESULTS.md#compile-time) |
| `simd/`, `simd_bytes/`, `half/`, `crypto/` | SIMD kernels, `f16`, byte search and decoding, HKDF/HMAC | |
| `any_matrix/`, `any_ops/`, `any_text/`, `key_order/`, `map_null/`, `union_view/`, `virtual_defaults/`, `array_view/`, `typed_array/`, `proxy/`, `ops/`, `inspect/`, `regex/`, `fs/`, `concurrent_map/`, `btree/`, `atomics/`, `fetch/`, `sigx/` | single-feature measurements (instruction counts or timings, some against Rust); each folder's script or program header says what it measures | |

## Performance docs

The result and findings files in `bench/` follow these rules:

1. **A results file holds only the latest measurement of each benchmark.** Each table states the
   machine, the date and the compiler commit it measured ("not recorded" when it is unknown).
   A new measurement replaces the old one; there is no running log of old numbers.
2. **FINDINGS holds only open findings.** A finding that is fixed is deleted; one that is partly
   fixed is rewritten to the gap that remains. Each is checked against the code when it is
   touched.
3. **Past optimization attempts are cut to their lasting lesson**, one or two lines (what was
   tried, why it did or did not pay), or removed.
4. **A dated snapshot file is deleted** once a newer snapshot of the same suite exists, unless
   something links to it as the current result (then that link moves to the newer one).
5. **A plan for work that is done** becomes a README of what exists, or is deleted.

Measure on a quiet machine, or compare by instructions retired (cachegrind on Linux,
`/usr/bin/time -l` on macOS) and interleave A/B runs, best of N; CONTRIBUTING.md has the
details.
