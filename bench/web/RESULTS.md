# bench/web results — TechEmpower-style server benchmarks

All six [TFB test types](https://github.com/TechEmpower/FrameworkBenchmarks/wiki/Project-Information-Framework-Tests-Overview)
(JSON, plaintext pipelined 16 deep, single query, multiple queries, fortunes, updates) for Velt
(`std/http` + `std/postgres`), Rust (axum 0.8 + tokio-postgres/deadpool), Go (`net/http` +
pgx/v5), Bun (`Bun.serve` + `Bun.SQL`) and Node (`node:http` + `pg`, one process and
`node:cluster`). Method, levels and deviations from the TFB rules: [README.md](README.md).
Raw data: `results/*.jsonl`; the tables come from `summarize.py` (each cell is the best req/s
over the measured connection levels, with that level's p99 and the server's peak RSS during the
test; ratios are to Rust).

## Lazy `Request` and Postgres batches — not re-run yet

The lazy Request and Postgres batch changes were measured while they were written, but the full suite has
**not** been re-run since: the machine was shared with other work (load average 10–38), and the
same route swung up to 2.4× between back-to-back wrk runs. The tables below are therefore still
the round 9 numbers. Re-run `run.sh` and `linux.sh` on a quiet machine before quoting them.

What was measured, interleaved (A/B/A/B) to cancel out the load:
- **Lazy `Request`** (`std/http` reads method/path/query/body/headers from the runtime on
  access; static per-thread `Server` header bytes): server CPU per request, `wrk -t4 -c64`,
  3 rounds. Raw req/s was too noisy to use.

  | test | Velt before | Velt after | Rust axum |
  |---|---|---|---|
  | JSON | 14.2–16.4 µs | 13.6–15.2 µs | 15.0–15.8 µs |
  | plaintext | 14.0–16.4 µs | 12.9–15.4 µs | 13.9–15.6 µs |
  | JSON, pipelined 16 deep | 2.22–2.39 µs | 1.90–2.07 µs | 8.4–8.9 µs |
  | plaintext, pipelined 16 deep | 2.21–2.31 µs | 1.82–1.94 µs | 8.5–8.75 µs |

  Per request, Velt is now at or below Rust on JSON (the brief's goal, in CPU terms).
- **Postgres batches with one `Sync`** (`batchQueryOne` in `/queries` and `/updates` from N ≥ 5),
  macOS, 512 connections, 6 interleaved 5 s rounds with the servers in rotated order, median
  req/s:

  | test | N | Velt | Velt before | Go (pgx) | Rust | Velt/Go |
  |---|---|---|---|---|---|---|
  | queries | 1 | 35100 | 35089 | 29116 | 35832 | 1.21 |
  | queries | 5 | 30796 | 23166 | 23438 | 23631 | 1.31 |
  | queries | 10 | 25757 | 15783 | 21509 | 16826 | 1.20 |
  | queries | 15 | 23766 | 12940 | 19985 | 13652 | 1.19 |
  | queries | 20 | 22330 | 10844 | 17251 | 11765 | 1.29 |
  | updates | 1 | 17184 | 17064 | 14614 | 17156 | 1.18 |
  | updates | 5 | 16685 | 13808 | 14499 | 14419 | 1.15 |
  | updates | 10 | 15710 | 11168 | 13778 | 11664 | 1.14 |
  | updates | 15 | 14480 | 9033 | 12545 | 9919 | 1.15 |
  | updates | 20 | 11812 | 7591 | 10876 | 7467 | 1.09 |

  Before, Go led at N ≥ 10 by 1.4–1.6×; now Velt leads Go at every N (goal: match or beat Go).
  At one connection a batch of 20 costs one round trip (215 µs vs 145 µs for N = 5).
- Linux arm64 was not measured for either change.

## Linux arm64 (Docker on Apple M4) — 2026-10-01

`bench/web/linux.sh --platform linux/arm64` at `a65d126`: Debian 12 containers in OrbStack's
Linux VM on an Apple M4 (10 cores, 32 GB), servers + wrk (4.1, `-t8`) in one container, Postgres
17 in another; 15 s per level after a 5 s warm-up, `DB_POOL=20` per process. Node 24.11.1, Bun
1.4.2, Go 1.27.1, rustc 1.98.1; Velt built with a release `velt` (LLVM, clang 19). The client,
the servers and the database share the 10 cores, as everywhere in this file. The other Mac tab
was idle (03:00–04:00); still, repeated runs of single tests varied by up to ±20% (JSON most).
Raw: `results/linux-arm64-2026-10-01-0244.{jsonl,md}`.

| test | velt | rust | go | bun | node | node-cluster |
|---|---:|---:|---:|---:|---:|---:|
| json | 740,998 (1.20×)<br>p99 3.3 ms, 49 MB | 619,154<br>p99 3.2 ms, 29 MB | 326,977 (0.53×)<br>p99 16.0 ms, 35 MB | 248,350 (0.40×)<br>p99 4.2 ms, 47 MB | 100,125 (0.16×)<br>p99 4.8 ms, 173 MB | 473,820 (0.77×)<br>p99 0.5 ms, 830 MB |
| plaintext | 3,308,530 (2.66×)<br>p99 12.6 ms, 54 MB | 1,244,042<br>p99 34.5 ms, 37 MB | 620,875 (0.50×)<br>p99 424.5 ms, 82 MB | 76,409 (0.06×)<br>p99 236.5 ms, 34 MB | 160,987 (0.13×)<br>p99 714.5 ms, 204 MB | 679,313 (0.55×)<br>p99 16.2 ms, 1362 MB |
| db | 141,215 (1.07×)<br>p99 1.0 ms, 25 MB | 132,573<br>p99 1.1 ms, 34 MB | 119,185 (0.90×)<br>p99 11.5 ms, 47 MB | 42,318 (0.32×)<br>p99 8.7 ms, 60 MB | 38,848 (0.29×)<br>p99 0.7 ms, 188 MB | 106,420 (0.80×)<br>p99 2.1 ms, 1345 MB |
| queries (N=1) | 131,173 (1.02×)<br>p99 4.8 ms, 43 MB | 128,977<br>p99 4.8 ms, 35 MB | 113,775 (0.88×)<br>p99 11.2 ms, 46 MB | 38,358 (0.30×)<br>p99 17.3 ms, 61 MB | 26,075 (0.20×)<br>p99 54.1 ms, 194 MB | 99,304 (0.77×)<br>p99 17.2 ms, 1666 MB |
| queries (N=5) | 64,540 (0.94×)<br>p99 9.5 ms, 43 MB | 68,488<br>p99 9.0 ms, 38 MB | 76,825 (1.12×)<br>p99 13.4 ms, 47 MB | 11,350 (0.17×)<br>p99 56.1 ms, 63 MB | 7,374 (0.11×)<br>p99 77.3 ms, 182 MB | 30,000 (0.44×)<br>p99 46.8 ms, 1560 MB |
| queries (N=10) | 42,561 (0.96×)<br>p99 14.6 ms, 44 MB | 44,253<br>p99 13.4 ms, 38 MB | 54,151 (1.22×)<br>p99 16.2 ms, 51 MB | 6,654 (0.15×)<br>p99 131.2 ms, 70 MB | 3,719 (0.08×)<br>p99 150.0 ms, 233 MB | 17,010 (0.38×)<br>p99 98.8 ms, 1564 MB |
| queries (N=15) | 31,611 (0.96×)<br>p99 18.9 ms, 48 MB | 32,765<br>p99 17.9 ms, 42 MB | 40,798 (1.25×)<br>p99 18.0 ms, 52 MB | 4,549 (0.14×)<br>p99 170.4 ms, 83 MB | 2,516 (0.08×)<br>p99 226.8 ms, 262 MB | 11,685 (0.36×)<br>p99 124.8 ms, 1559 MB |
| queries (N=20) | 25,001 (0.97×)<br>p99 23.5 ms, 50 MB | 25,807<br>p99 23.1 ms, 41 MB | 32,848 (1.27×)<br>p99 20.4 ms, 53 MB | 3,636 (0.14×)<br>p99 255.4 ms, 87 MB | 1,896 (0.07×)<br>p99 335.3 ms, 274 MB | 8,877 (0.34×)<br>p99 168.3 ms, 1566 MB |
| fortunes | 114,719 (0.97×)<br>p99 1.3 ms, 28 MB | 118,108<br>p99 1.1 ms, 40 MB | 89,251 (0.76×)<br>p99 12.8 ms, 47 MB | 33,339 (0.28×)<br>p99 21.4 ms, 94 MB | 23,046 (0.20×)<br>p99 3.4 ms, 278 MB | 89,601 (0.76×)<br>p99 2.5 ms, 1584 MB |
| updates (N=1) | 48,389 (0.98×)<br>p99 12.5 ms, 51 MB | 49,170<br>p99 12.4 ms, 44 MB | 47,989 (0.98×)<br>p99 18.2 ms, 47 MB | 39,234 (0.80×)<br>p99 16.4 ms, 104 MB | 17,851 (0.36×)<br>p99 100.7 ms, 276 MB | 41,242 (0.84×)<br>p99 30.5 ms, 1528 MB |
| updates (N=5) | 33,292 (0.96×)<br>p99 18.1 ms, 47 MB | 34,719<br>p99 17.1 ms, 45 MB | 35,593 (1.03×)<br>p99 22.2 ms, 48 MB | 16,374 (0.47×)<br>p99 42.2 ms, 101 MB | 5,740 (0.17×)<br>p99 121.3 ms, 166 MB | 20,062 (0.58×)<br>p99 58.7 ms, 1358 MB |
| updates (N=10) | 24,748 (0.95×)<br>p99 24.4 ms, 46 MB | 25,920<br>p99 23.0 ms, 42 MB | 30,916 (1.19×)<br>p99 24.1 ms, 48 MB | 9,900 (0.38×)<br>p99 60.2 ms, 96 MB | 3,217 (0.12×)<br>p99 216.0 ms, 190 MB | 12,114 (0.47×)<br>p99 96.9 ms, 1539 MB |
| updates (N=15) | 19,402 (0.95×)<br>p99 30.0 ms, 53 MB | 20,458<br>p99 28.5 ms, 41 MB | 24,054 (1.18×)<br>p99 27.5 ms, 50 MB | 7,298 (0.36×)<br>p99 79.4 ms, 97 MB | 2,260 (0.11×)<br>p99 278.9 ms, 193 MB | 8,371 (0.41×)<br>p99 136.7 ms, 1525 MB |
| updates (N=20) | 15,782 (0.94×)<br>p99 37.9 ms, 54 MB | 16,835<br>p99 34.5 ms, 42 MB | 19,528 (1.16×)<br>p99 35.8 ms, 51 MB | 5,746 (0.34×)<br>p99 105.2 ms, 98 MB | 1,685 (0.10×)<br>p99 416.9 ms, 201 MB | 6,402 (0.38×)<br>p99 169.8 ms, 1530 MB |

**Velt vs Rust: 0.94–1.07× on every database test, 1.20× on JSON, 2.66× on pipelined
plaintext**, at a peak RSS of 25–54 MB (Rust 29–45 MB, Go 35–53 MB, Bun 34–104 MB, Node
173–278 MB, Node cluster 0.8–1.7 GB). Go leads on `queries`/`updates` with N ≥ 5 (see
"Remaining differences"). p99 latencies of Velt and Rust are close on the DB tests (1.0 vs 1.1 ms on
`/db`, 23.5 vs 23.1 ms at 20 queries).


## macOS arm64 (bare metal, Apple M4) — 2026-10-01

`bench/web/run.sh` at `a65d126` directly on macOS 26.6 (Apple M4, 10 cores, 32 GB), wrk 4.2.0
(kqueue, `-t8`), same levels and durations as above; Postgres 17 in Docker (OrbStack), so every
query crosses the VM boundary: the DB tests are bound by that round trip on every server, and
their absolute numbers are ~3× lower than on Linux. Raw: `results/macos-2026-10-01-0333.{jsonl,md}`.

| test | velt | rust | go | bun | node | node-cluster |
|---|---:|---:|---:|---:|---:|---:|
| json | 205,079 (1.06×)<br>p99 6.6 ms, 40 MB | 193,799<br>p99 5.7 ms, 49 MB | 188,018 (0.97×)<br>p99 11.0 ms, 43 MB | 182,777 (0.94×)<br>p99 0.7 ms, 54 MB | 113,323 (0.58×)<br>p99 0.3 ms, 103 MB | 186,463 (0.96×)<br>p99 28.1 ms, 988 MB |
| plaintext | 2,846,000 (5.15×)<br>p99 3.5 ms, 42 MB | 552,577<br>p99 10.1 ms, 49 MB | 428,452 (0.78×)<br>p99 331.0 ms, 91 MB | 34,249 (0.06×)<br>p99 120.2 ms, 57 MB | 183,786 (0.33×)<br>p99 1110.0 ms, 236 MB | 443,440 (0.80×)<br>p99 30.6 ms, 1158 MB |
| db | 40,904 (1.01×)<br>p99 3.2 ms, 64 MB | 40,549<br>p99 8.7 ms, 93 MB | 33,353 (0.82×)<br>p99 42.6 ms, 91 MB | 46,996 (1.16×)<br>p99 197.1 ms, 68 MB | 31,037 (0.77×)<br>p99 3.8 ms, 239 MB | 29,779 (0.73×)<br>p99 4.2 ms, 1528 MB |
| queries (N=1) | 39,393 (1.00×)<br>p99 17.6 ms, 64 MB | 39,413<br>p99 17.4 ms, 94 MB | 32,279 (0.82×)<br>p99 21.1 ms, 92 MB | 45,478 (1.15×)<br>p99 199.9 ms, 69 MB | 28,885 (0.73×)<br>p99 30.6 ms, 246 MB | 28,391 (0.72×)<br>p99 39.3 ms, 1575 MB |
| queries (N=5) | 26,922 (0.96×)<br>p99 24.7 ms, 65 MB | 27,968<br>p99 30.3 ms, 95 MB | 29,442 (1.05×)<br>p99 31.3 ms, 92 MB | 15,245 (0.55×)<br>p99 441.4 ms, 78 MB | 8,022 (0.29×)<br>p99 92.4 ms, 198 MB | 7,165 (0.26×)<br>p99 126.2 ms, 1424 MB |
| queries (N=10) | 18,918 (0.92×)<br>p99 49.1 ms, 65 MB | 20,654<br>p99 40.0 ms, 96 MB | 27,134 (1.31×)<br>p99 22.6 ms, 92 MB | 9,806 (0.47×)<br>p99 446.1 ms, 99 MB | 4,048 (0.20×)<br>p99 168.1 ms, 276 MB | 3,752 (0.18×)<br>p99 315.2 ms, 1642 MB |
| queries (N=15) | 15,654 (0.96×)<br>p99 62.6 ms, 65 MB | 16,368<br>p99 71.3 ms, 96 MB | 24,351 (1.49×)<br>p99 24.5 ms, 92 MB | 6,879 (0.42×)<br>p99 466.2 ms, 126 MB | 2,117 (0.13×)<br>p99 319.2 ms, 297 MB | 2,520 (0.15×)<br>p99 729.6 ms, 1951 MB |
| queries (N=20) | 12,589 (0.93×)<br>p99 82.7 ms, 65 MB | 13,494<br>p99 113.4 ms, 97 MB | 21,359 (1.58×)<br>p99 28.8 ms, 92 MB | 5,457 (0.40×)<br>p99 588.7 ms, 144 MB | 1,474 (0.11×)<br>p99 499.8 ms, 310 MB | 1,864 (0.14×)<br>p99 566.4 ms, 1954 MB |
| fortunes | 38,202 (0.99×)<br>p99 2.8 ms, 65 MB | 38,689<br>p99 9.5 ms, 98 MB | 31,984 (0.83×)<br>p99 68.5 ms, 92 MB | 40,861 (1.06×)<br>p99 205.9 ms, 150 MB | 25,968 (0.67×)<br>p99 3.7 ms, 313 MB | 28,116 (0.73×)<br>p99 49.9 ms, 1876 MB |
| updates (N=1) | 18,996 (0.98×)<br>p99 34.9 ms, 65 MB | 19,404<br>p99 33.1 ms, 99 MB | 16,525 (0.85×)<br>p99 40.8 ms, 92 MB | 27,690 (1.43×)<br>p99 336.8 ms, 155 MB | 15,733 (0.81×)<br>p99 51.6 ms, 313 MB | 14,265 (0.74×)<br>p99 72.0 ms, 1428 MB |
| updates (N=5) | 14,623 (0.94×)<br>p99 45.8 ms, 65 MB | 15,479<br>p99 40.8 ms, 99 MB | 15,231 (0.98×)<br>p99 40.3 ms, 92 MB | 12,586 (0.81×)<br>p99 497.6 ms, 156 MB | 6,193 (0.40×)<br>p99 121.2 ms, 209 MB | 5,613 (0.36×)<br>p99 165.2 ms, 1429 MB |
| updates (N=10) | 11,686 (0.94×)<br>p99 62.5 ms, 65 MB | 12,450<br>p99 57.4 ms, 99 MB | 14,804 (1.19×)<br>p99 40.5 ms, 92 MB | 7,971 (0.64×)<br>p99 747.3 ms, 156 MB | 3,504 (0.28×)<br>p99 186.5 ms, 232 MB | 3,221 (0.26×)<br>p99 245.6 ms, 1769 MB |
| updates (N=15) | 9,677 (0.96×)<br>p99 81.5 ms, 65 MB | 10,113<br>p99 77.7 ms, 99 MB | 13,370 (1.32×)<br>p99 47.1 ms, 92 MB | 6,225 (0.62×)<br>p99 525.2 ms, 167 MB | 2,413 (0.24×)<br>p99 292.2 ms, 238 MB | 2,275 (0.22×)<br>p99 377.4 ms, 1959 MB |
| updates (N=20) | 8,276 (0.93×)<br>p99 91.5 ms, 65 MB | 8,923<br>p99 82.8 ms, 99 MB | 12,043 (1.35×)<br>p99 49.2 ms, 92 MB | 4,982 (0.56×)<br>p99 623.5 ms, 178 MB | 1,766 (0.20×)<br>p99 350.8 ms, 243 MB | 1,740 (0.20×)<br>p99 464.1 ms, 1961 MB |

**Velt vs Rust: 0.92–1.06× on every database test, 1.06× on JSON, 5.15× on pipelined
plaintext** (macOS loopback makes per-write costs high, so batching pipelined responses matters
even more than on Linux). Bun leads `db`/`queries N=1`/`updates N=1` here with a very long p99
tail (200–340 ms); Go leads N ≥ 10 as on Linux.


## Findings and fixes (in the order they were found)

Every gap the preliminary runs showed against Rust is explained below; the runtime-side causes
were fixed on this branch, so none of the final numbers is more than 1.3× behind Rust.

1. **Preliminary Velt numbers were measured on the debug runtime.** `velt` links the
   `libvelt_rt.a` next to its own executable, so a server built by `target/debug/velt` runs on
   the debug runtime (debug assertions, the checked `DebugAlloc` wrapper; it showed up in a
   `sample` profile). All final numbers use a release `velt`; run.sh now warns about a debug
   one. Since round 11, `velt build --release` / `velt run --release` warn when they link a debug
   runtime library, and `velt doctor` says which build it found.
2. **A handler whose client hung up was cancelled, leaking pooled connections** (found by the
   benchmark agent: after the first wrk run the pool was empty and every DB route hung). hyper
   drops a request's future when its connection closes; a `pool.connect()` client held by that
   handler is a Copy struct with no drop hook, so it never went back. Fixed in the runtime
   (`http/server.rs`): an unfinished handler is moved to a task of its own and runs to
   completion, as in Node; the response is discarded. Regression tests: rt ABI test
   `a_handler_finishes_after_its_client_left`, golden `std/postgres_pool_client_left`.
3. **Pipelined plaintext: one `writev` per response.** The profile of the Velt server under
   `wrk -s pipeline.lua -- 16` was ~70% `writev`: hyper flushed every response separately.
   Enabling hyper's `pipeline_flush` (responses to pipelined requests are written together)
   took pipelined plaintext on macOS from 453k to 2.2M req/s (non-pipelined throughput and
   latency unchanged within noise). `half_close(true)` came with it: without it, a client that
   pipelines requests and then shuts down its write side (`printf … | nc`) lost the last
   response (all of them with `pipeline_flush`); the rt ABI test
   `pipelined_requests_are_all_answered_even_after_a_half_close` covers both.
4. **The database state leaked between servers.** Each server's `/updates` runs leave dead row
   versions in `world`, so later servers' queries were slower (Rust looked 3× behind Velt on
   fortunes in one macOS run, Go 2× in another). run.sh now runs `VACUUM (FULL, ANALYZE)` and a
   `CHECKPOINT` before each server.
5. **Rust baseline without `TCP_NODELAY`.** `axum::serve` leaves Nagle's algorithm on, which
   stalled its pipelined plaintext on Linux (87k req/s); the baseline now sets it through
   `ListenerExt::tap_io`, like the Velt runtime and Go do by default.
6. **`/updates?queries=1` 0.76× Rust on Linux** (the only gap > 1.3×): the Velt server checked
   out a pooled connection twice per request (one `pool.queryOne` for the read, one
   `pool.execute` for the write); Rust does both on one connection. Doing the same (one
   `pool.connect()` client per `/updates` request) made it 1.14× Rust. Not a runtime cost:
   two acquisitions per request simply halve the pool's capacity at 512 connections.
7. **No fast PRNG in std.** The server drew random ids with one OS `getrandom` call per
   request (`std/crypto.randomBytes`). Added `std/random` (`random()`, `randomInt(min, max)`:
   a per-thread wyrand generator seeded from the OS; one multiply per number).
8. **Request headers: O(n²) and one ABI call per header.** std built `req.headers` with one
   `header_at(i)` call per header, each walking the header map from the start. Two calls now
   return all names and all values (`velt_rt_http_req_header_names` / `_values`). Small for
   wrk's two headers; it matters for browser-sized requests (15–20 headers).

Remaining differences, not runtime bugs:
- **Go is ahead on `queries`/`updates` with N ≥ 5** (1.1–1.4× Rust and Velt): pgx sends a
  `pgx.Batch` as one pipelined message group with a single `Sync`, so the N reads cost one
  round trip; tokio-postgres (Rust and Velt) pipelines the N queries too but each has its own
  `Sync`, and the server answers them one by one. Fix direction (Velt and Rust alike): a batch
  API in std/postgres that sends N executions of one prepared statement with one `Sync`
  (tokio-postgres has no public API for it; it would need the raw protocol layer).
- **JSON is the one place Velt trails Rust** (0.8–1.2× across runs; the two are within
  run-to-run noise of each other on Linux): per request, std/http copies the method, path,
  query, body and every header into Velt strings before the handler runs (`requestFromRaw`),
  and the response object, the `Server` header and the serialized body are three more
  allocations. A lazily materialized `Request` (accessors that read the runtime request on
  demand) would remove most of it; that needs a `Request` that owns the runtime handle and a
  drop hook, which is a std/http API change worth doing together with the hot-reload
  constraints of §13.5.
- **Bun's pipelined plaintext is low** (58k req/s; ~230k without pipelining): Bun's own
  behaviour with 16-deep pipelining, not the script (checked by hand).
- **Node in one process** saturates one core (and shows socket errors at 1024 connections on
  pipelined plaintext); `node:cluster` uses 10 processes and ~1.2–1.9 GB.
