# TechEmpower-style web benchmarks

The six [TechEmpower Framework Benchmarks](https://github.com/TechEmpower/FrameworkBenchmarks/wiki/Project-Information-Framework-Tests-Overview)
test types, served by Velt (`std/http` + `std/postgres`) and by the usual baselines, against
Postgres 17 with the TFB `hello_world` schema. `bench/http/techempower` is the DB-less
predecessor (plaintext, JSON and an in-memory fortunes page).

```
db/init.sql, db/db.sh   Postgres 17 in Docker (container velt-web-pg) with World + Fortune
velt/server.vlt        std/http + std/postgres (createPool); the routes are in velt/app.ts
velt-tsx/server.vlt    the same routes, the fortunes page in TSX (velt:jsx, precompiled)
node/server.mjs         node:http + pg (Pool); CLUSTER=1: node:cluster, one worker per core
bun/server.ts           Bun.serve + Bun.SQL (Bun's built-in Postgres client)
go/main.go              net/http + pgx/v5 (pgxpool)
rust/src/main.rs        axum 0.8 + tokio-postgres (deadpool-postgres), release + thin LTO
run.sh                  builds, verifies every route, loads every test with wrk
linux.sh                the same in a Linux container (servers + wrk) next to a Postgres one
summarize.py            one Markdown row per test from a results .jsonl (best level, × Rust)
measure.py              route verification, peak-RSS sampling, wrk output parsing
pipeline.lua            wrk script for HTTP/1.1 pipelining (TFB's plaintext test)
results/                raw results (OUT.jsonl + OUT.md per run)
```

## Running

```sh
bench/web/db/db.sh up          # PGPORT=5433 if a local postgres already has 5432
bench/web/run.sh               # full run: ~40 min for all servers
QUICK=1 bench/web/run.sh       # smoke run: 5 s per level, fewer levels
SERVERS="velt rust" TESTS="db fortunes" bench/web/run.sh
bench/web/db/db.sh down

bench/web/linux.sh --platform linux/arm64          # everything in Docker, no local setup
bench/web/summarize.py bench/web/results/<run>.jsonl
```

**Use a release `velt`** (`cargo build --release -p veltc -p velt_rt`, then
`VELT=target/release/velt`): `velt` links the runtime library that sits next to it, so a debug
`velt` produces servers on the *debug* runtime (debug assertions, checked allocator), which
are several times slower. run.sh warns when `VELT` is a debug build.

Needs `wrk`, `python3`, `curl`, Docker for the database, and the toolchain of every server in
`SERVERS`: a `velt` compiler (`VELT=…`; `VELT_STD` defaults to this checkout's `std/`), Node +
npm, Bun, Go, cargo. All the knobs (`DURATION`, `CONC`, `QUERY_COUNTS`, `PGPORT`, `DB_POOL`, …)
are listed at the top of `run.sh`. It works with macOS' bash 3.2 and on Linux (a Debian
container needs `wrk`, `python3`, `curl`, `procps`; without `ps`, RSS is read from `/proc`).

Every server takes the port as `argv[1]` (or `PORT`), listens on `HOST` (default 127.0.0.1),
reads `DATABASE_URL` (default
`postgres://benchmarkdbuser:benchmarkdbpass@127.0.0.1:5432/hello_world`) and `DB_POOL`, the
number of database connections of the process (default **2 × cores** everywhere; `node-cluster`
splits it between its workers, at least 2 each).

## What run.sh does

1. Builds each server (Velt `velt build --release`, which uses LLVM when clang is found; Go
   `go build`; Rust `cargo build --release`; Node `npm ci` once).
2. Resets the database (`VACUUM FULL` of `world`, `CHECKPOINT`; needs `psql`) so every
   server starts from the same state: the previous server's updates leave dead rows behind.
   Then starts one server at a time and **verifies** it (`measure.py verify`): plaintext and JSON
   bodies; `Server`, `Date` and exact `Content-Type` headers on every route; `/db` and
   `/queries` row shape and range; `queries` handling (missing, empty, `foo`, `0` → 1 row,
   `501` → 500 rows); the `/fortunes` HTML byte for byte against the expected page (so it is
   identical across implementations); and that `/updates?queries=20` really persisted the
   returned numbers (through `psql` on `DATABASE_URL`, or `docker exec` into the container).
   A failed check is reported and makes the run exit 1, but the server is measured anyway.
3. Loads each test with `wrk --latency` after a warm-up run at the same level:

   | test | path | levels (connections) |
   |---|---|---|
   | JSON | `/json` | 16, 64, 256, 512 |
   | plaintext | `/plaintext` | 256, 1024, pipelined 16 deep (`pipeline.lua`) |
   | single query | `/db` | 16, 64, 256, 512 |
   | multiple queries | `/queries?queries=N` | 512, N = 1, 5, 10, 15, 20 |
   | fortunes | `/fortunes` | 16, 64, 256, 512 |
   | updates | `/updates?queries=N` | 512, N = 1, 5, 10, 15, 20 |

   `QUICK=1`: 64 and 512 connections, plaintext at 256, N = 1 and 20, 5 s runs, 2 s warm-up.
4. Records req/s, average and p99 latency, wrk socket errors and non-2xx responses, and the peak
   RSS of the server process tree (sampled every 100 ms during the measured run; the node-cluster
   figure is the primary plus all workers) as one JSON object per measurement in `OUT.jsonl`
   and as a Markdown table row in `OUT.md` (and on stdout).

## Implementation notes

All implementations follow the same shape, so the comparison is about the stack:

- `/db` and `/queries` read one row per query with a prepared
  `SELECT id, randomnumber FROM world WHERE id = $1`. The N queries of a request run
  concurrently: pipelined on one pooled connection where the client can (Velt: from N = 5 a
  `pool.batchQueryOne` batch with one `Sync`, like pgx's; below, a dedicated `pool.connect()`
  client with the queries joined by `Promise.all`; Go: a `pgx.Batch`; Rust:
  `try_join_all` on one deadpool client), or spread over the pool (`pg` has no pipelining, so
  Node's `Promise.all` uses up to N connections; Bun.SQL distributes and pipelines by itself).
- `/updates` reads N rows as above, gives each a new random number, and writes them with **one**
  statement (TFB allows batched updates):
  `UPDATE world SET randomnumber = u.r FROM (SELECT unnest($1::int[]) AS id, unnest($2::int[]) AS r) AS u WHERE world.id = u.id`,
  with the arrays sorted by id so concurrent requests lock rows in the same order. Velt and Bun
  bind the arrays as `{1,2,3}` text (std/postgres has no array parameters); the others bind
  native arrays. A request may draw the same id twice; the response then holds both numbers
  while the table keeps one (the persistence check only looks at ids that appear once).
- `/fortunes` queries all rows, adds the request-time fortune, sorts by message (code-point
  order), and renders the table with the same five entities everywhere (`&amp; &lt; &gt; &quot;
  &#39;`; Go writes the page with a `strings.Replacer` because `html/template` writes `&#34;`).
- `/json` serializes a fresh object per request; `/plaintext` returns a constant string.
- Headers: every response carries `Server` (set by the handler) and `Date` (added by hyper,
  node:http, Bun and net/http), and an exact `Content-Type`.

## Deviations from the TFB rules

- One machine runs the database, the server and wrk (TFB uses three), so they compete for
  cores; numbers are for relative comparison only.
- Postgres runs in Docker with `max_connections=2000`, `shared_buffers=256MB` and
  `synchronous_commit=off` (TFB-like; TFB's exact config differs in details).
- wrk runs with `min(cores, 8)` threads rather than TFB's fixed setup, and without TFB's
  "primer" and 60 s runs (15 s here).
- Velt draws random ids from `std/random` (a per-thread wyrand, added for this benchmark; the
  others use their fast PRNGs). An earlier version used one OS read per request and ran the DB
  work in a spawned task, working around a runtime bug (a handler whose client hung up was
  cancelled, and a pooled client it held never went back to the pool); the runtime now
  finishes such handlers, like Node.
- Bun runs as one process. `REUSE_PORT=1` sets `reusePort`, so several `bun server.ts` processes
  can share the port on Linux; run.sh does not start that mode.
