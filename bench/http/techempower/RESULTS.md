# TechEmpower-style HTTP benchmark

Routes (all servers produce byte-identical bodies):
- `/plaintext` → `Hello, World!` (`text/plain; charset=utf-8`)
- `/json` → `{"message":"Hello, World!"}` (`application/json`, serialized per request)
- `/fortunes` → DB-less fortunes: the 12 TechEmpower fortunes built per request (in place of the
  query), one added at request time, sorted by message, rendered to an HTML table with escaping.

Servers: `server.vlt` (`velt build --release`, LLVM, std/http on hyper), `server.mjs` on Node
24.11.1 `node:http` as one process and with `CLUSTER=1` (`node:cluster`, one worker per core),
and an axum 0.8 baseline (`axum/`, tokio multi-thread, release + thin LTO, system allocator).
Run with `bench/http/techempower/run.sh` (knobs in its header): `oha -z 10s -c 256`, 2 s warm-up
per route, HTTP/1.1 keep-alive, client and server on the same machine (loopback).

## 2026-09-30 — Apple M4 (4P + 6E cores, 32 GB), macOS 26.6.2, oha 1.16.0

**Noisy machine:** two other agents were compiling the whole time (load average 20–32 on 10
cores). The script was run twice with 2 runs per point; the table shows the best of those 4
runs (with its avg / p99) and the range of all 4. Treat differences under ~20% as noise.

| server | route | req/s (best of 4) | avg latency | p99 | range of 4 runs |
|---|---|---|---|---|---|
| Velt | /plaintext | 194,511 | 1.31 ms | 11.8 ms | 146k–195k |
| Velt | /json | 195,965 | 1.30 ms | 11.8 ms | 130k–196k |
| Velt | /fortunes | 131,634 | 1.94 ms | 8.0 ms | 82k–132k |
| axum 0.8 | /plaintext | 185,433 | 1.38 ms | 9.9 ms | 132k–185k |
| axum 0.8 | /json | 161,758 | 1.58 ms | 16.0 ms | 126k–162k |
| axum 0.8 | /fortunes | 161,105 | 1.59 ms | 10.0 ms | 117k–161k |
| Node 24 cluster (10 workers) | /plaintext | 152,535 | 1.68 ms | 25.5 ms | 87k–153k |
| Node 24 cluster (10 workers) | /json | 162,152 | 1.58 ms | 22.9 ms | 84k–162k |
| Node 24 cluster (10 workers) | /fortunes | 128,211 | 1.99 ms | 18.6 ms | 80k–128k |
| Node 24 (1 process) | /plaintext | 98,596 | 2.59 ms | 6.0 ms | 14k–99k |
| Node 24 (1 process) | /json | 74,845 | 3.42 ms | 7.7 ms | 25k–75k |
| Node 24 (1 process) | /fortunes | 60,799 | 4.21 ms | 7.7 ms | 32k–61k |

A shorter trial run earlier (1 s runs, somewhat lower load) gave Velt 214k /json and 167k
/fortunes, axum 195k / 179k, Node 114k / 73k, Node cluster 168k / 102k.

Observations:
- /plaintext and /json: Velt is at parity with axum (both hyper underneath) and 2–2.6× a single
  Node process; Node needs all 10 cores (`cluster`) to get near, with 2× worse p99.
- /fortunes: Velt is ~20% behind axum. The Velt handler escapes each message with five
  `replaceAll` calls and builds the page with template literals + `join`, allocating a string per
  step; axum escapes in one pass into a preallocated `String`. A single-pass escaper (or a string
  builder in std) would close most of the gap.
- p99 of every multi-threaded server is inflated by the client competing for the same cores.

Known deviations from the TechEmpower rules: no database (fortunes are in memory), no `Server`
header. (The runs above served the Velt `/fortunes` as `text/plain`; it now uses
`Response.html`, added afterwards, which changes only the header.)
