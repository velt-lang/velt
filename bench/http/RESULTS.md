# HTTP server throughput (`velt_rt_http_serve`)

Handler: the state machine generated code emits for `(req) => Response.text("Hello, World!")`,
written in Rust against the C ABI (`crates/velt_rt/tests/abi/http_bench.rs`). Load: HTTP/1.1
keep-alive, one request in flight per connection, 5 s, client and server on the same machine
(loopback).

```sh
cargo test -p velt_rt --release --lib http_bench -- --ignored --nocapture
# knobs: BENCH_CONNS BENCH_SECS BENCH_CLIENT_THREADS VELT_THREADS
#        BENCH_TARGET=127.0.0.1:3001 (measure another server)   BENCH_OHA=1 (also run oha)
```

Baselines: plain hyper 1.x (same `hyper_util::server::conn::auto` builder as the runtime) and
axum 0.8 hello-world servers, `#[tokio::main]`, release + thin LTO, measured with the same client.

## 2026-09-30 — i9-12900HK (14C/20T), Windows 11, loopback

| server | 64 conns, 4 client threads | 256 conns, 8 client threads | oha, 256 conns | 1 server worker, 256 conns |
|---|---|---|---|---|
| velt_rt | 55.8k req/s | 63.2k req/s | 62.8k req/s | 15.0k–17.2k req/s |
| hyper | 27.7k–41.4k req/s | 53.5k req/s | 52.3k req/s | 17.0k req/s |
| axum | 31.2k–53.5k req/s | 46.7k req/s | 51.2k req/s | 16.0k req/s |

The runtime's request path is at parity with plain hyper: per core (one worker) all three are
within run-to-run noise, and the ~60 µs per request per core is dominated by Windows loopback
socket I/O, not by the runtime. Per request the runtime adds three small allocations on top of
hyper's (the `VeltReq` and `VeltResp` boxes, the copied static body); the handler state lives inline
in the request future. The runtime links mimalloc; the baselines use the system allocator, which is
likely why velt_rt is slightly ahead with many workers.

## End-to-end: compiled Velt program vs Node (after M4)
`examples/http_hello.velt` built with `velt build --release` (LLVM) vs an equivalent Node 22 `http`
server; `oha -z 10s -c 256` on the same machine (client competes for CPU), Windows 11, i9-12900HK.

| server | req/s | avg latency | p99 | peak RSS under load |
|---|---|---|---|---|
| Velt (compiled, `serve` + shared counter) | 45,170 | 5.6 ms | 26.1 ms | 17.8 MB |
| Node 22 `http` | 13,046 | 19.6 ms | 38.6 ms | 69.4 MB |

(The rt-only numbers above, with a Rust client, put hyper/axum at 47–53k req/s on this machine.)

## End-to-end on Apple silicon (stream A, 2026-09-30)
Same setup as above on an Apple M4 (4P + 6E cores), macOS 26.6: `examples/http_hello.velt` built
with `velt build --release` (LLVM, Apple clang 21) vs the same Node server on Node 24.11.1
(`http.createServer`, one process). `oha -z 10s -c 256` on the same machine; peak RSS from
`/usr/bin/time -l`. Three runs each; the ranges are run-to-run spread.

| server | req/s | avg latency | p99 | peak RSS under load |
|---|---|---|---|---|
| Velt (compiled, `serve` + shared counter) | 219k–222k | 1.15 ms | 9.0–9.5 ms | 18.5–20.7 MB |
| Node 24 `http` | 128k–135k | 1.9–2.0 ms | 3.6–3.7 ms | 129–130 MB |

Velt serves 1.7× Node's throughput in a seventh of the memory. With the default worker count its
p99 looks worse than Node's, but that is an artifact of running the client on the same machine.
Varying `VELT_THREADS` (same binary and load) shows it:

| Velt workers | req/s | p50 | p99 | p99.9 |
|---|---|---|---|---|
| 10 (default = cores) | 218k | 0.79 ms | 7.4 ms | 18.1 ms |
| 6 | 219k | 1.02 ms | 4.0 ms | 8.1 ms |
| 4 | 218k | 1.08 ms | 3.3 ms | 6.6 ms |
| 2 | 217k | 1.16 ms | 2.2 ms | 3.2 ms |
| 1 | 207k | 1.22 ms | 1.6 ms | 2.6 ms |

Throughput is flat, so oha, not the server, is the bottleneck. The tail comes from
oversubscription: ten workers plus oha's threads on ten cores (six of them efficiency cores)
means workers get descheduled, and their connections stall for a scheduler slice. A single Velt
worker still does 207k req/s with a 1.6 ms p99, better than single-threaded Node (~130k, 3.6 ms)
on every metric. With the load generator on a separate machine this does not arise.

## 2026-11-11: step 0 of #2726 (#2729), Linux builder

**Machine:** Intel i9-8950HK (6 cores / 12 threads, a laptop part; `powersave` governor, turbo
on), 30 GB, Omarchy Linux 7.2.8, oha 1.16.0, rustc 1.99.1. Velt at `e1757fe41` (main of
2026-11-11; #2734, step 1, not merged). Client and server on the same machine, pinned with
`taskset` to disjoint CPUs: **server on CPUs 0,1,6,7** (physical cores 0 and 1 with their
hyperthreads), **oha on CPUs 2-5**. CPUs 8-11 (the hyperthreads of cores 2-5) ran another
compile at nice 19 during all the runs below, so the client shared its cores with it, and the
clock varies with the package's total load. That makes req/s noisy: the spread between the
best-of-3 runs is often 20-40%, well over 5%, so **read req/s differences under ~30% as
noise**. The instruction counts are not affected. "Load" is the 1-minute average before each
round and includes the benchmark's own server and client (about 4-6 by themselves).

**Method.** `bench/http/techempower/scaling.sh`, `bench/http/modlock/run.sh` and
`bench/http/middleware/run.sh` (helpers in `bench/http/lib.sh`), all with
`SERVER_CPUS=0,1,6,7 CLIENT_CPUS=2,3,4,5`. Each round measures every server once (1 s warm-up,
then `oha -z 5s -c 256`, HTTP/1.1 keep-alive); the rounds are interleaved, and the best req/s of
3 is reported with that run's p99. Workers: `VELT_THREADS`, `TOKIO_WORKER_THREADS` (axum),
xitca's `worker_threads`. With 4 CPUs for the server, 8 workers is oversubscribed.
**Instructions per request:** cachegrind (`--cache-sim=no`) on the server with one worker, run
twice, serving 2,000 and 12,000 requests on 8 connections: (Ir(12k) - Ir(2k)) / 10k, so startup
and shutdown cancel out. "Velt std" is the same program compiled with `VELT_SCHEDULER=std` (the
executor in Velt). On this commit that executor runs one worker (#2706, its workers, is not
merged), so its 4- and 8-worker rows are one worker too.

### TechEmpower routes by worker count

xitca-web 0.8.3 (`techempower/xitca`, release + thin LTO) serves byte-identical bodies to axum
(md5 per route). Velt's bodies are the same; its `text/plain` header is
`text/plain;charset=UTF-8`.

| workers | server | /plaintext req/s | p99 | /json req/s | p99 | /fortunes req/s | p99 |
|---|---|---|---|---|---|---|---|
| 1 | Velt | 66,317 | 5.87 ms | 54,416 | 6.17 ms | 50,664 | 7.25 ms |
| 1 | Velt std | 57,821 | 5.95 ms | 53,198 | 8.40 ms | 47,304 | 8.47 ms |
| 1 | axum 0.8 | 76,745 | 5.21 ms | 75,475 | 5.96 ms | 59,618 | 5.72 ms |
| 1 | xitca-web | 92,639 | 4.03 ms | 84,403 | 5.80 ms | 63,287 | 5.67 ms |
| 4 | Velt | 64,109 | 8.15 ms | 65,104 | 5.64 ms | 52,929 | 6.16 ms |
| 4 | Velt std | 58,395 | 6.33 ms | 54,723 | 6.85 ms | 43,484 | 7.89 ms |
| 4 | axum 0.8 | 159,613 | 3.78 ms | 151,363 | 4.15 ms | 111,664 | 4.18 ms |
| 4 | xitca-web | 161,422 | 5.85 ms | 195,372 | 5.06 ms | 133,735 | 3.32 ms |
| 8 | Velt | 67,315 | 5.67 ms | 65,063 | 6.54 ms | 48,166 | 8.36 ms |
| 8 | Velt std | 56,964 | 7.79 ms | 55,269 | 6.52 ms | 42,413 | 8.17 ms |
| 8 | axum 0.8 | 153,140 | 4.48 ms | 129,522 | 5.55 ms | 107,644 | 6.45 ms |
| 8 | xitca-web | 155,780 | 7.15 ms | 157,222 | 6.79 ms | 124,141 | 9.18 ms |

Load before the three rounds: 1 worker 5.8, 5.9, 5.5; 4 workers 5.7, 8.8, 9.6; 8 workers 9.7,
11.4, 11.0.

Instructions per request (one worker):

| server | /plaintext | /json | /fortunes |
|---|---|---|---|
| Velt | 15,248 | 15,912 | 37,885 |
| Velt std | 13,897 | 16,544 | 36,699 |
| axum 0.8 | 13,189 | 13,631 | 41,779 |
| xitca-web | 9,496 | 9,885 | 37,981 |

- **Per core, Velt costs what axum costs:** 15-16k instructions per request against axum's
  13-14k on /plaintext and /json, and fewer on /fortunes (37.9k against 41.8k, with the
  single-pass escape). xitca-web needs about a third fewer on the small routes. With one worker,
  Velt's req/s is 14-28% behind axum's in these rounds, at the edge of the noise: a first round
  before the neighbouring compile started (server then on CPUs 2-5) gave Velt 88,007 /plaintext
  against axum's 78,742 and xitca's 77,349.
- **Velt doesn't scale past one core.** axum and xitca double from 1 to 4 workers; Velt stays at
  its 1-worker throughput on both schedulers. It isn't CPU-bound: with 4 workers the server used
  about 85% of one CPU (busy share of its threads over a 5 s run, from
  `/proc/<pid>/task/*/stat`: 41%, 33%, 6%, 4%). Sampling the threads' `wchan` 200 times during a
  run found the workers in `ep_poll` 745 times and running 55 times, never in a futex wait. So
  the workers are idle, not blocked on a lock: about one worker at a time serves the
  connections. tokio's schedulers in the same binary behave the same way
  (`VELT_SCHEDULER=stealing`: 38,752 req/s, busy 71%, 9%, 6%; `per-core`: 43,927, busy 82%;
  these runs overlapped a cachegrind run on the server CPUs, so compare them only with each
  other). This server reads no module state (its `--emit vir` has no `velt_rt_module_state`
  call), so the module lock is not the cause.
- **Velt std against the default:** at one worker the executor in Velt is 2-13% behind on req/s
  (inside this run's noise), at about the same instruction count (13.9-36.7k against 15.2-37.9k).

### Module lock

`modlock/server.velt`: each request does 200 lookups in a 32-entry `Map<string, string>`.
`module` reads a module-level `const cache`, which compiles to one `velt_rt_module_state` call
per handler call; `param` uses the same map made in `main` and captured by the handler.

| workers | scheduler | module req/s | p99 | param req/s | p99 | load |
|---|---|---|---|---|---|---|
| 1 | Velt | 31,584 | 4.63 ms | 32,117 | 2.17 ms | 3.6-4.0 |
| 1 | Velt std | 32,355 | 4.26 ms | 30,938 | 11.93 ms | 3.6-4.0 |
| 4 | Velt | 28,641 | 7.29 ms | 25,746 | 16.53 ms | 4.2-4.4 |
| 4 | Velt std | 28,173 | 5.19 ms | 29,829 | 12.68 ms | 4.2-4.4 |
| 8 | Velt | 25,416 | 6.54 ms | 23,738 | 17.54 ms | 5.2-6.2 |
| 8 | Velt std | 24,457 | 18.65 ms | 21,913 | 18.10 ms | 5.2-6.2 |

Instructions per request (one worker): Velt module 106,055, param 100,656; Velt std module
105,644, param 99,125.

**Neither variant scales, so this row can't yet separate the module lock from the placement
problem above:** the server without module state stays on one core too. The module-state variant
costs 5-6% more instructions per request (the lock and the state lookup). The `param` count
includes the per-request copy of the handler's environment (the 32-entry map). The row has to be
repeated once connections are spread over the workers (step 2).

### Skewed load

The same server (`param`): 6 keep-alive connections on `/light` and 2 on `/heavy` (10x the
lookups) at the same time for 5 s, with the busy share of each server thread over the run
(threads under 2% left out). A WebSocket-heavy mix was not run.

| workers | scheduler | /light req/s | p99 | /heavy req/s | p99 | thread busy share | load |
|---|---|---|---|---|---|---|---|
| 4 | Velt | 6,450 | 1.43 ms | 2,148 | 1.43 ms | 78%, 16% | 6.8 |
| 4 | Velt std | 6,870 | 1.17 ms | 2,290 | 1.17 ms | 95% | 6.7 |
| 8 | Velt | 9,222 | 1.00 ms | 3,073 | 1.00 ms | 95% | 6.8 |
| 8 | Velt std | 9,008 | 0.98 ms | 3,001 | 0.98 ms | 95% | 6.6 |

All 8 connections run on about one thread. Every connection, light or heavy, gets the same
request rate (about 1,075 per second at 4 workers) and the same p99, so a light request waits its
turn behind the heavy ones.

### Middleware and soak

`middleware/server.velt`: 0, 1 or 3 middleware of the `next` shape in #2726, called through a
function value; the innermost `next` reads `req`. req/s at 4 workers.

| middleware | instructions per request | req/s | p99 | peak RSS | load |
|---|---|---|---|---|---|
| 0 | 15,363 | 76,766 | 4.68 ms | 19 MiB | 5.1-5.4 |
| 1 | 109,020 | 38,775 | 9.08 ms | 20 MiB | 5.1-5.4 |
| 3 | 198,611 | 28,804 | 12.02 ms | 24 MiB | 5.1-5.4 |

**Cost per hop:** the first middleware adds 93.7k instructions per request (6.1 times the bare
request's 15.4k), each further one 44.8k ((198,611 - 109,020) / 2). That is the per-call copy of
what `next` captured (the request), the number step 1 must bring down.

**Soak** (`middleware/run.sh`: 100k requests to 3 middleware, 4 workers, 64 connections, RSS
every 10k). Three runs were flat by the script's rule (the second half's growth over the first
half's peak: -18, 4 and 8 bytes per request, against a bound of 256), ending at 23.0, 13.4 and
15.7 MiB. The RSS grows in steps of a few MiB over the first ~50k requests (the allocator's
heaps), so two earlier rules with a fixed 4 MiB slack failed 3 of 4 runs (ending at 17.3-24.6
MiB). 300k-request runs level off: 0 middleware at 13.1 MiB, 3 middleware at
20.4 MiB (4 workers) and 15.4 MiB (1 worker). There is no per-request leak like #2719's (40 KB
per request and hop would be 12 GB here).
