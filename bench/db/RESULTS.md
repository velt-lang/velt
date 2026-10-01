# bench/db results

Method, workloads and deviations: `README.md`. Command: `bench/db/run.sh --runs 5` (full sizes,
best of 5 interleaved rounds, after one untimed run whose op counts and checksums all matched
Rust's).

- **Machine**: Apple M4 (10 cores), 32 GB, macOS 26.6.2. **Busy**: other agents were building
  and testing on it (load average 8–17 during the run), so single runs varied up to 2–4× on the
  syscall- and round-trip-bound workloads; best-of-5 interleaved rounds hide most of that, but
  treat differences under ~20% as noise.
- **Servers** (local, TCP on localhost): PostgreSQL 17.11 (Homebrew), Redis 8.8.1.
- **Toolchains**: rustc 1.98.1; Velt at `2b01113` (stream/db with std/postgres), `velt build
  --release` on LLVM and Cranelift; Node 24.11.1.
- **Libraries**: rusqlite 0.40.2 (bundled SQLite, the same crate and version as the Velt
  runtime), redis 1.7.1 (tokio-comp, `MultiplexedConnection`), tokio-postgres 0.7.18 +
  deadpool-postgres 0.14.2, tokio 1.53.1; better-sqlite3 12.11.1, ioredis 5.11.1, pg 8.23.1.

## Summary vs Rust

| backend | Velt LLVM vs Rust | notes |
|---|---|---|
| SQLite | 0.29–0.91× | only `range_select` is > 2× slower (3.4×); Node 0.46–0.83× |
| Redis | 0.98–1.83× | at or above Rust everywhere; 2.5–3× Node on pipelines and concurrency |
| Postgres | 0.83–1.33× | round-trip bound: at Rust's level; `range_select` 0.83× |

Cranelift is within noise of LLVM on every workload here: the time goes to the runtime, SQLite
and the network, not to compiled Velt code.

## Root causes

- **`sqlite.range_select` (0.29×, the one > 2× gap): the JSON row round trip.** Rows travel as
  JSON text from the runtime to std, then `JSON.parse<Row[]>` decodes them. A `sample` profile of
  the Velt LLVM loop (100 rows per query, 3 columns): SQLite itself (`sqlite3_step`) is 24% of
  the time; encoding the rows to JSON in `velt_rt::sqlite::rows::read` is ~36%, of which 15% is
  formatting the REAL column (`velt_rt::fmt::push_f64` → `ryu::format64` plus JS-style fix-ups
  and memmoves); `JSON.parse` in the compiled program is ~34% (`velt_rt_json_reader_*`,
  `Scanner::string`/`number`, `velt_rt_str_eq` for field-name matching, `dec2flt` to parse the
  float back). Rust reads each column straight into the struct, so its whole query costs about
  what `sqlite3_step` alone costs in Velt (≈ 9 µs vs 32 µs per 100-row query). Node's
  better-sqlite3 builds JS objects directly from the columns (0.46×). Fix direction: a
  compile-time row decoder that reads typed columns from the runtime directly (or at least a
  binary row format), or, as a cheap step, writing REAL columns with ryu's output as is.
- **`sqlite.insert_tx` (0.60–0.78×)**: per row, `JSON.stringify` of the params object in Velt
  (including float formatting of `score`, 7% of samples) and `db_json::parse_params` in the
  runtime (17%), then named binding; `sqlite3_step` is ~40%. Same JSON-in-the-middle cost as
  above, smaller because rows are short and there is no decode side.
- **`sqlite.point_select` (0.66×)**: ~59% of Velt' samples are the OS calls SQLite makes for
  every statement (WAL read-lock `fcntl` and page `pread`s on cache misses), which Rust makes
  too; the rest is the JSON row + parse, the connection lock and the `prepare_cached` lookup
  (`StatementCache::get`, ~1%). Under this machine's load the syscall part dominates and swings
  a lot (single Rust runs ranged 1.5–3.7 s for this loop), so the ratio is only indicative.
- **`sqlite.insert_autocommit` (≈ 1×)**: commit-bound (WAL append per row); overheads vanish.
- **Redis**: std/redis' own RESP2 client (one writer task batching queued requests into one
  write, one reader completing reply slots) appears to do less per command than the `redis` crate's
  `MultiplexedConnection` (not profiled; that crate converts every reply through its generic
  `Value` and `FromRedisValue`), hence 1.5–1.8× on sequential round trips and ≥ 1× on pipelines; `concurrent_get`
  matches the multi-thread Rust baseline (Rust's current-thread runtime is 1.3× faster there,
  since 50 tasks on one connection gain nothing from cross-thread wake-ups).
- **Postgres**: every workload is round-trip bound (~30–40 µs per query on loopback), so the
  client libraries mostly wait on the server. `range_select` (0.83×) is the one where decoding
  100 rows per query shows: the same JSON row round trip as SQLite, diluted by the round trip.
  The pool workloads match Rust (deadpool) and beat pg's pool by ~1.3×.
- **Memory**: Velt programs peak at 10–11 MB vs Rust's 4–8 MB and Node's 108–211 MB.

## Full table

| workload | implementation | ops/s | × Rust |
|---|---|---:|---:|
| sqlite.insert_tx | Rust | 1,560,338 | 1.00 |
| sqlite.insert_tx | Velt LLVM | 936,568 | 0.60 |
| sqlite.insert_tx | Velt Cranelift | 1,220,587 | 0.78 |
| sqlite.insert_tx | Node | 1,102,444 | 0.71 |
| sqlite.point_select | Rust | 622,605 | 1.00 |
| sqlite.point_select | Velt LLVM | 408,755 | 0.66 |
| sqlite.point_select | Velt Cranelift | 447,019 | 0.72 |
| sqlite.point_select | Node | 474,093 | 0.76 |
| sqlite.range_select | Rust | 107,202 | 1.00 |
| sqlite.range_select | Velt LLVM | 31,506 | 0.29 |
| sqlite.range_select | Velt Cranelift | 34,427 | 0.32 |
| sqlite.range_select | Node | 49,166 | 0.46 |
| sqlite.insert_autocommit | Rust | 124,282 | 1.00 |
| sqlite.insert_autocommit | Velt LLVM | 112,586 | 0.91 |
| sqlite.insert_autocommit | Velt Cranelift | 134,039 | 1.08 |
| sqlite.insert_autocommit | Node | 103,379 | 0.83 |
| redis.set_seq | Rust | 33,752 | 1.00 |
| redis.set_seq | Rust current-thread | 37,632 | 1.11 |
| redis.set_seq | Velt LLVM | 61,786 | 1.83 |
| redis.set_seq | Velt Cranelift | 59,588 | 1.77 |
| redis.set_seq | Node | 26,685 | 0.79 |
| redis.get_seq | Rust | 43,470 | 1.00 |
| redis.get_seq | Rust current-thread | 39,704 | 0.91 |
| redis.get_seq | Velt LLVM | 63,922 | 1.47 |
| redis.get_seq | Velt Cranelift | 63,854 | 1.47 |
| redis.get_seq | Node | 35,714 | 0.82 |
| redis.pipeline_set | Rust | 1,043,964 | 1.00 |
| redis.pipeline_set | Rust current-thread | 1,014,155 | 0.97 |
| redis.pipeline_set | Velt LLVM | 1,202,503 | 1.15 |
| redis.pipeline_set | Velt Cranelift | 1,304,237 | 1.25 |
| redis.pipeline_set | Node | 398,038 | 0.38 |
| redis.pipeline_get | Rust | 1,243,364 | 1.00 |
| redis.pipeline_get | Rust current-thread | 1,113,228 | 0.90 |
| redis.pipeline_get | Velt LLVM | 1,566,005 | 1.26 |
| redis.pipeline_get | Velt Cranelift | 1,584,534 | 1.27 |
| redis.pipeline_get | Node | 497,866 | 0.40 |
| redis.concurrent_get | Rust | 614,101 | 1.00 |
| redis.concurrent_get | Rust current-thread | 796,955 | 1.30 |
| redis.concurrent_get | Velt LLVM | 604,324 | 0.98 |
| redis.concurrent_get | Velt Cranelift | 481,892 | 0.78 |
| redis.concurrent_get | Node | 262,972 | 0.43 |
| postgres.insert_tx | Rust | 26,895 | 1.00 |
| postgres.insert_tx | Rust current-thread | 27,863 | 1.04 |
| postgres.insert_tx | Velt LLVM | 32,939 | 1.22 |
| postgres.insert_tx | Velt Cranelift | 34,472 | 1.28 |
| postgres.insert_tx | Node | 24,646 | 0.92 |
| postgres.point_select | Rust | 26,429 | 1.00 |
| postgres.point_select | Rust current-thread | 29,708 | 1.12 |
| postgres.point_select | Velt LLVM | 35,227 | 1.33 |
| postgres.point_select | Velt Cranelift | 38,251 | 1.45 |
| postgres.point_select | Node | 24,532 | 0.93 |
| postgres.range_select | Rust | 14,976 | 1.00 |
| postgres.range_select | Rust current-thread | 15,623 | 1.04 |
| postgres.range_select | Velt LLVM | 12,403 | 0.83 |
| postgres.range_select | Velt Cranelift | 12,129 | 0.81 |
| postgres.range_select | Node | 10,009 | 0.67 |
| postgres.pool_select_8 | Rust | 81,612 | 1.00 |
| postgres.pool_select_8 | Rust current-thread | 87,683 | 1.07 |
| postgres.pool_select_8 | Velt LLVM | 79,178 | 0.97 |
| postgres.pool_select_8 | Velt Cranelift | 79,830 | 0.98 |
| postgres.pool_select_8 | Node | 58,942 | 0.72 |
| postgres.pool_select_64 | Rust | 77,312 | 1.00 |
| postgres.pool_select_64 | Rust current-thread | 82,350 | 1.07 |
| postgres.pool_select_64 | Velt LLVM | 75,143 | 0.97 |
| postgres.pool_select_64 | Velt Cranelift | 75,914 | 0.98 |
| postgres.pool_select_64 | Node | 57,060 | 0.74 |

Best of 5 runs, full sizes. × Rust = ops/s ÷ Rust's ops/s (below 1 is slower).

| program | implementation | peak RSS MB (whole run) |
|---|---|---:|
| sqlite | Rust | 5.5 |
| sqlite | Velt LLVM | 10.4 |
| sqlite | Velt Cranelift | 10.3 |
| sqlite | Node | 108.5 |
| redis | Rust | 4.4 |
| redis | Rust current-thread | 3.2 |
| redis | Velt LLVM | 9.7 |
| redis | Velt Cranelift | 9.6 |
| redis | Node | 210.8 |
| postgres | Rust | 8.0 |
| postgres | Rust current-thread | 7.6 |
| postgres | Velt LLVM | 11.4 |
| postgres | Velt Cranelift | 11.3 |
| postgres | Node | 138.0 |
