# Database clients — Velt vs Node and Rust

Inserts and queries per second through Velt' `std/sqlite`, `std/redis` and `std/postgres`,
against the usual Node drivers (`better-sqlite3`, `ioredis`, `pg`) and Rust crates (`rusqlite`,
`redis`, `tokio-postgres` + `deadpool-postgres`). Run everything with `bench/db/run.sh` (see
`--help`); results are in `RESULTS.md`.

```
run.sh            builds everything, validates, runs best-of-N and prints the Markdown tables
measure.py        the part of run.sh that runs the programs and compares/tabulates their output
velt/<backend>.vlt        sqlite, redis, postgres
node/<backend>.mjs         + common.mjs, package.json / package-lock.json (`npm ci` by run.sh)
rust/src/bin/<backend>.rs  + src/lib.rs; standalone Cargo project (not a workspace member)
```

## Servers

SQLite needs nothing (a file database in the temp directory, deleted afterwards). Postgres and
Redis come from `BENCH_PG_URL` / `BENCH_REDIS_URL`, falling back to the goldens'
`VELT_TEST_PG_URL` / `VELT_TEST_REDIS_URL`; a backend without a URL shows as `n/a`. Every run
uses its own table (`bench_db_<lang>_<unique>`, dropped at the end) or key prefix
(`<lang>-bench-db:<unique>:`, deleted at the end; never `FLUSHALL`).

## Method

- Each program runs every workload of its backend in order and prints
  `RESULT <backend.workload> <ops> <checksum> <ms>`. The time comes from an in-process monotonic
  clock (`performance.now()`, `Instant`) around the timed loop only, so process startup,
  connecting, schema setup and statement preparation are excluded.
- `run.sh` runs every program once untimed; each workload's op count and checksum must equal the
  Rust program's (otherwise that cell is `n/a` with a note). Then it runs each program `--runs`
  times (default 3) and keeps each workload's best ops/s. `× Rust` is ops/s ÷ Rust's ops/s
  (below 1 is slower). Peak RSS is per program run (all workloads of a backend), the smallest
  over the timed runs.
- `--quick` runs 1/20 of the sizes (a correctness pass; the numbers mean little).
- Velt is built with `velt build --release` on the LLVM and the Cranelift backend; Rust in
  release with LTO. Rust's async programs run twice: on tokio's multi-thread runtime (the `Rust`
  row and the × Rust reference, the same runtime shape as Velt') and on the current-thread
  runtime (`Rust current-thread`, one thread like Node's event loop).
- Rows are decoded into a typed object in every language: `class Row { id; name; score }` (a
  class instance in Velt and Node, a struct in Rust). Checksums are `id + name.length +
  trunc(score * 2)` summed over rows, or the sum of `changes` / OK replies / value lengths.
- Keys, ids and the range starts are scattered with `(i * 7919) % n`, the same in every language.

## Workloads

SQLite: file database, `journal_mode = WAL`, `synchronous = NORMAL`, table
`bench (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL NOT NULL)`. All statements use
named parameters (`:id`) bound from an object, prepared once before the loop.

| workload | what | ops |
|---|---|---:|
| `sqlite.insert_tx` | one prepared `INSERT` per row, all inside one `BEGIN`/`COMMIT` | 1,000,000 rows |
| `sqlite.point_select` | `SELECT id, name, score … WHERE id = :id`, decoded into a `Row` | 1,000,000 |
| `sqlite.range_select` | `… WHERE id >= :lo AND id < :hi` (100 rows), decoded into `Row[]` | 50,000 queries |
| `sqlite.insert_autocommit` | one prepared `INSERT` per row, no transaction (a commit each) | 100,000 rows |

Redis: 20,000 distinct keys `<prefix><j>` holding `value-<j>`; one multiplexed connection.

| workload | what | ops |
|---|---|---:|
| `redis.set_seq` | `SET`, awaited one at a time (round-trip bound) | 40,000 |
| `redis.get_seq` | `GET`, awaited one at a time | 40,000 |
| `redis.pipeline_set` | `SET` in pipelines of 100 commands | 1,000,000 |
| `redis.pipeline_get` | `GET` in pipelines of 100 commands | 1,000,000 |
| `redis.concurrent_get` | 50 tasks issuing sequential `GET`s over the one shared client | 400,000 |

Postgres: table `(id INTEGER PRIMARY KEY, name TEXT NOT NULL, score DOUBLE PRECISION NOT NULL)`.
Sequential workloads use one connection; statements are prepared (named statements in `pg`,
`prepare` / `prepare_cached` in tokio-postgres, std/postgres' per-connection statement cache).

| workload | what | ops |
|---|---|---:|
| `postgres.insert_tx` | prepared `INSERT … VALUES ($1, $2, $3)` per row inside one transaction | 20,000 rows |
| `postgres.point_select` | prepared `SELECT … WHERE id = $1`, decoded into a `Row` | 20,000 |
| `postgres.range_select` | `… WHERE id >= $1 AND id < $2` (100 rows) into `Row[]` | 5,000 queries |
| `postgres.pool_select_8` | point selects from 8 concurrent tasks over a pool of 8 connections | 100,000 |
| `postgres.pool_select_64` | the same from 64 concurrent tasks (tasks wait for a connection) | 100,000 |

## Deviations

- **Node rows** are wrapped into `new Row(...)` from the driver's plain object (better-sqlite3 and
  pg return plain objects); Velt decodes straight into the class (via JSON), Rust into a struct.
- **SQLite transactions** use explicit `BEGIN`/`COMMIT` everywhere (Velt `db.begin()` +
  `tx.commit()`, `db.exec("BEGIN")` in Node, `execute_batch` in Rust) rather than each driver's
  closure helper, so the SQL sent is identical.
- **Postgres ids** are `INTEGER` (int4), since `pg` returns `BIGINT` as a string; Rust reads
  `i32` and widens it.
- **Postgres pools**: `pg.Pool({ max: 8 })` (`pool.query`: check out, query, release) and
  deadpool-postgres (`pool.get()` + `prepare_cached` per query). Both pools are filled with 8
  open connections before timing. Each connection runs one query at a time in both.
- **Redis** in Rust uses the `redis` crate's `MultiplexedConnection` (cloned into each task);
  ioredis and std/redis multiplex one connection per client by design. The Rust SET checks for
  an OK reply as `Option<String>`, ioredis compares with `"OK"`, Velt' `set()` returns a bool.
- **Velt Postgres insert** binds an object to `VALUES (:id, :name, :score)`: std/postgres takes
  an array only when its values share one type, and std rewrites `:name` to `$n` once per
  cached statement, so the server sees the same `$1, $2, $3` statement as Node and Rust. The
  Velt pool is `createPool({ url, max: 8 })` with `pool.queryOne` (a free connection per
  query), warmed by holding 8 `pool.connect()` clients before timing.
- **Velt transactions** in Postgres use `c.begin()` / `tx.commit()` (tokio-postgres:
  `client.transaction()`; pg: `BEGIN` / `COMMIT` queries).
