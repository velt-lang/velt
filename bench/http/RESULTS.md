# HTTP server benchmarks

Velt's HTTP servers (the `http-server` package on `velt/net`) against axum 0.8 and xitca-web
0.8.3. The scripts:

| script | measures |
|---|---|
| `techempower/scaling.sh` | `/plaintext`, `/json`, `/fortunes` of `techempower/server.velt`, axum and xitca-web by worker count |
| `modlock/run.sh` | a handler reading module state against the same handler with captured state |
| `middleware/run.sh` | 0, 1 or 3 middleware of the `next` shape, plus a soak (RSS over 100k requests) |
| `serve_cost/run.sh` | instructions per request of one `serve` handler shape, base against head |
| `tls/run.sh` | HTTPS `/plaintext`, instructions per request and req/s |
| `count.sh` | instructions per request of the techempower and web servers, for comparing two builds |
| `compare.ps1` | A/B of two compilers on `examples/http_hello.velt` with oha, plus an optional soak |
| `fetch/` | the HTTP client ([fetch/README.md](fetch/README.md)) |
| `techempower/run.sh` | the database-free TechEmpower routes ([techempower/RESULTS.md](techempower/RESULTS.md)) |

Shared helpers are in `lib.sh`. All results below were measured before native programs ran on
the executor in Velt: the "Velt" rows ran on the runtime's tokio-based executor, and the
"Velt std" rows on the executor in Velt, which then had one worker. `techempower/scaling.sh` and
`modlock/run.sh` still build a `velt-std` server with `VELT_SCHEDULER=std`, which nothing reads
any more: both of their Velt servers are now the same program on the executor in Velt.

## Machine and method

Intel i9-8950HK (6 cores / 12 threads, a laptop part; `powersave` governor, turbo on), 30 GB,
Linux 7.2.8, oha 1.16.0, rustc 1.99.1. Client and server on the same machine, pinned with
`taskset` to disjoint CPUs: server on CPUs 0,1,6,7 (physical cores 0 and 1 with their
hyperthreads), oha on CPUs 2–5 (`SERVER_CPUS=0,1,6,7 CLIENT_CPUS=2,3,4,5`). Each round measures
every server once (1 s warm-up, then `oha -z 5s -c 256`, HTTP/1.1 keep-alive); rounds are
interleaved, and the best req/s of 3 is reported. Other builds ran on the remaining CPUs, so
req/s differences under about 30% are noise; instruction counts are not affected. Workers:
`VELT_THREADS`, `TOKIO_WORKER_THREADS` (axum), xitca's `worker_threads`; with 4 CPUs for the
server, 8 workers is oversubscribed.

Instructions per request: cachegrind (`--cache-sim=no`) on the server with one worker, run twice,
serving 2,000 and 12,000 requests on 8 connections: (Ir(12k) − Ir(2k)) / 10k, so startup and
shutdown cancel out.

## Velt by worker count

2026-11-11, commit ac6d91007. Each connection is a task of its own, pinned to the workers in
turn (`spawn(connection(...), { worker })`), when `TcpListener.workers()` is above 1. In a
program with module state `spreadsConnections()` is false (a constant the compiler knows), only
the one-task accept loop is built, and its connections stay on one worker: a task that touched
module state takes the process-wide module lock at the start of every later poll, so spreading
them would add a lock handoff to every request ([#2726](https://github.com/velt-lang/velt/issues/2726)).
`techempower/server.velt` has no module state; `modlock/server.velt` has (its `module` mode's
`cache`), so both its modes take the one-task path. Load average 2.4–10.8.

| workers | /plaintext req/s | /json req/s | modlock `module` req/s | modlock `param` req/s |
|---|---:|---:|---:|---:|
| 1 | 58,177 | 56,051 | 27,824 | 26,670 |
| 4 | 113,928 | 116,068 | 26,395 | 25,864 |
| 8 | 121,615 | 117,165 | 30,592 | 31,355 |

Instructions per request (one worker; measured next to other builds):

| /plaintext | /json | modlock `module` | modlock `param` |
|---:|---:|---:|---:|
| 16,793 | 17,130 | 107,345 | 102,493 |

`VELT_RESCUE=0` (no stall monitor) changes these by under 1.5%.

## Against axum and xitca-web

2026-11-11, commit e1757fe41. xitca-web (`techempower/xitca`, release + thin LTO) serves
byte-identical bodies to axum (md5 per route); Velt's bodies are the same, and its `text/plain`
header is `text/plain;charset=UTF-8`. Velt's rows by worker count are in the section above; at
this commit its connections stayed on one worker.

| workers | server | /plaintext req/s | p99 | /json req/s | p99 | /fortunes req/s | p99 |
|---|---|---|---|---|---|---|---|
| 1 | Velt | 66,317 | 5.87 ms | 54,416 | 6.17 ms | 50,664 | 7.25 ms |
| 1 | Velt std | 57,821 | 5.95 ms | 53,198 | 8.40 ms | 47,304 | 8.47 ms |
| 1 | axum 0.8 | 76,745 | 5.21 ms | 75,475 | 5.96 ms | 59,618 | 5.72 ms |
| 1 | xitca-web | 92,639 | 4.03 ms | 84,403 | 5.80 ms | 63,287 | 5.67 ms |
| 4 | axum 0.8 | 159,613 | 3.78 ms | 151,363 | 4.15 ms | 111,664 | 4.18 ms |
| 4 | xitca-web | 161,422 | 5.85 ms | 195,372 | 5.06 ms | 133,735 | 3.32 ms |
| 8 | axum 0.8 | 153,140 | 4.48 ms | 129,522 | 5.55 ms | 107,644 | 6.45 ms |
| 8 | xitca-web | 155,780 | 7.15 ms | 157,222 | 6.79 ms | 124,141 | 9.18 ms |

Instructions per request (one worker):

| server | /plaintext | /json | /fortunes |
|---|---|---|---|
| Velt | 15,248 | 15,912 | 37,885 |
| Velt std | 13,897 | 16,544 | 36,699 |
| axum 0.8 | 13,189 | 13,631 | 41,779 |
| xitca-web | 9,496 | 9,885 | 37,981 |

Per core, Velt costs about what axum costs: 14–16k instructions per request against axum's
13–14k on `/plaintext` and `/json`, and fewer on `/fortunes` (single-pass escaping). xitca-web
needs about a third fewer on the small routes. With 4 workers Velt reaches about 114k req/s on
`/plaintext` (section above), against 160k for axum and xitca-web.

## Middleware and soak

`middleware/server.velt`: 0, 1 or 3 middleware of the `next` shape in
[#2726](https://github.com/velt-lang/velt/issues/2726), called through a function value; the
innermost `next` reads `req`. 4 workers, 2026-11-11, commit e1757fe41, load 5.1–5.4.

| middleware | instructions per request | req/s | p99 | peak RSS |
|---|---|---|---|---|
| 0 | 15,363 | 76,766 | 4.68 ms | 19 MiB |
| 1 | 109,020 | 38,775 | 9.08 ms | 20 MiB |
| 3 | 198,611 | 28,804 | 12.02 ms | 24 MiB |

The first middleware adds 93.7k instructions per request and each further one 44.8k: the per-call
copy of what `next` captured (the request).

Soak (`middleware/run.sh`: 100k requests to 3 middleware, 4 workers, 64 connections, RSS every
10k): flat by the script's rule (the second half's growth over the first half's peak, against a
bound of 256 bytes per request) in three runs, ending at 13–23 MiB. RSS grows in steps of a few
MiB over the first ~50k requests (the allocator's heaps); 300k-request runs level off at 13–20
MiB. There is no per-request leak.

## Lessons

- **A started promise stays on its task.** Handing an already started promise to `spawn` does
  not move it to a new task; each connection must be spawned from the direct call
  (`spawn(connection(...))`) to run on another worker.
- **Module state keeps a server on one worker** until the module lock is replaced
  ([#2726](https://github.com/velt-lang/velt/issues/2726)); pass such state to the handler as a
  captured value instead.
- **Leave out the branch a constant rules out.** velt_opt's inliner has one growth budget for
  the whole program; a second instance of the connection state machine in a branch that never
  runs used it up before the handler's loop was inlined, so `spreadsConnections()` is a
  compile-time constant and the other branch is not built.
- **A client on the same machine inflates p99.** With as many server workers as cores plus the
  load generator's threads, workers get descheduled and their connections stall for a
  scheduler slice; fewer workers cut the tail at the same throughput.
