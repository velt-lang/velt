# bench/web results: TechEmpower-style server benchmarks

All six [TFB test types](https://github.com/TechEmpower/FrameworkBenchmarks/wiki/Project-Information-Framework-Tests-Overview)
(JSON, plaintext pipelined 16 deep, single query, multiple queries, fortunes, updates) for Velt,
Rust (axum 0.8 + tokio-postgres/deadpool), Go (`net/http` + pgx/v5), Bun (`Bun.serve` +
`Bun.SQL`) and Node (`node:http` + `pg`, one process and `node:cluster`). Method, levels and
deviations from the TFB rules: [README.md](README.md). Raw data: `results/*.jsonl`; the tables come
from `summarize.py` (each cell is the best req/s over the measured connection levels, with that
level's p99 and the server's peak RSS during the test; ratios are to Rust).

Both runs below predate the Velt servers' move to the `http-server` package (they ran on the
runtime's hyper-based HTTP server); the suite has not been run on `http-server` yet.

**Linux arm64 in CI:** `gh workflow run bench-arm -f suite=web -f ref=<branch>` runs the
fortunes test for `velt` and `velt-tsx` on a GitHub `ubuntu-24.04-arm` runner (4 cores shared by
Postgres 17 in a service container, the servers and wrk), with the summary in the run and the
raw `.jsonl` / `.md` as the `bench-web-arm64` artifact. Its req/s compare the servers within one
run; they are not comparable with the table below.

## Azure D48ads_v5, pinned

AMD EPYC 7763, Linux x86_64, 48 cores; each server pinned to 8 physical cores (`SERVER_CPUS`),
Postgres to 4 cores with their SMT siblings, wrk to the rest (`CLIENT_CPUS`); 10 s runs,
`wrk -t16`, `DB_POOL=16`. 2026-10-10, commit not recorded. Raw rows and table:
`results/azure-d48ads_v5-pinned-20261010.*` (*velt-shards* there is a prototype per-core
scheduler build, not one of `run.sh`'s servers).

- **JSON, 64 connections:** velt-shards 556k req/s, velt 345k, Rust/axum 346k, Node cluster
  243k, Go 229k, Bun 63k, Node 30k.
- **Plaintext, pipelined:** velt 3.27–3.34M, Rust 737k, Go 399k.
- **Database tests** are bound by the 16-connection pool's round trips (Postgres at 3.5–3.9 of
  its 8 vCPUs): single query velt 51k against Go and Rust 54k, fortunes 46k against 51k,
  queries×20 33k (Go 31k, Rust 15k).

## Fortunes in TSX (`velt-tsx`)

The `velt-tsx` server is `velt` with the fortunes page written in TSX (`velt-tsx/fortunes.tsx`,
precompiled by `velt/jsx`) instead of template literals. Both pass `measure.py`'s byte-exact
fortunes check (the TSX page writes `'` as `&#x27;`, as react-dom does). The bench-arm workflow
on a GitHub `ubuntu-24.04-arm` runner (4 cores shared by Postgres 17, the servers and wrk; both
servers verified), 2026-10-09, commit be741dcd; best req/s over the levels 16/64/256/512. Raw
data: `results/linux-arm64-tsx-2026-10-09-1232.*`.

| velt (template literals) | velt-tsx (TSX) | velt-tsx / velt |
|---:|---:|---:|
| 16 515 (16 conn) | 16 284 (16 conn) | 0.99 |

The test is bound by the database (rendering the page is about 1% of a request's CPU time), so
it shows that TSX costs nothing measurable here, not how fast it renders:
[bench/jsx](../jsx/README.md) measures the rendering itself.

## Lessons from running the suite

- **Use a release `velt`.** A debug `velt` links the debug runtime (debug assertions, checked
  allocator), which is several times slower; `run.sh` warns about it.
- **Reset the database between servers.** Each server's `/updates` leaves dead row versions in
  `world`, which slows the next server's queries (by up to 3× in one run); `run.sh` runs
  `VACUUM (FULL, ANALYZE)` and a `CHECKPOINT` before each server.
- **`TCP_NODELAY` everywhere.** `axum::serve` leaves Nagle's algorithm on, which stalls
  pipelined responses; the Rust baseline sets it like the other servers.
- **One connection per `/updates` request.** Checking out a pooled connection twice per request
  (read, then write) halves the pool's capacity at 512 connections.
- **Batches with one `Sync`.** Sending the N reads of `/queries` and `/updates` as one batch with
  a single `Sync` (pgx's `pgx.Batch`, the postgres package's `batchQueryOne`) costs one round
  trip; separate `Sync`s make the server answer them one by one, which is where tokio-postgres
  falls behind pgx from N = 5.
- **Pipelined responses are written together.** A server that writes each response to a
  pipelined request separately spends most of its time in `writev`.
