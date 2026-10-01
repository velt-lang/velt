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
`examples/http_hello.vlt` built with `velt build --release` (LLVM) vs an equivalent Node 22 `http`
server; `oha -z 10s -c 256` on the same machine (client competes for CPU), Windows 11, i9-12900HK.

| server | req/s | avg latency | p99 | peak RSS under load |
|---|---|---|---|---|
| Velt (compiled, `serve` + shared counter) | 45,170 | 5.6 ms | 26.1 ms | 17.8 MB |
| Node 22 `http` | 13,046 | 19.6 ms | 38.6 ms | 69.4 MB |

(The rt-only numbers above, with a Rust client, put hyper/axum at 47–53k req/s on this machine.)

## End-to-end on Apple silicon (stream A, 2026-09-30)
Same setup as above on an Apple M4 (4P + 6E cores), macOS 26.6: `examples/http_hello.vlt` built
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
