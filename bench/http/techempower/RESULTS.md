# TechEmpower-style HTTP benchmark (no database)

Routes (all servers produce byte-identical bodies):
- `/plaintext` → `Hello, World!`
- `/json` → `{"message":"Hello, World!"}` (`application/json`, serialized per request)
- `/fortunes` → database-free fortunes: the 12 TechEmpower fortunes built per request (in place
  of the query), one added at request time, sorted by message, rendered to an HTML table with
  escaping (`escapeHtml`, one pass).

Servers: `server.velt` (`velt build --release`, LLVM, the `http-server` package's `serve`),
`server.mjs` on Node `node:http` as one process and with `CLUSTER=1` (`node:cluster`, one worker
per core), an axum 0.8 baseline (`axum/`, tokio multi-thread, release + thin LTO, system
allocator) and a xitca-web 0.8.3 baseline (`xitca/`).

- `run.sh` (knobs in its header: `DURATION`, `CONNS`, `RUNS`, `ROUTES`, `SERVERS`) loads Velt,
  Node and axum with `oha -z 10s -c 256` after a 2 s warm-up per route, HTTP/1.1 keep-alive,
  client and server on the same machine (loopback), and prints one Markdown row per server and
  route.
- `scaling.sh` measures the routes by worker count (1, 4, 8) for Velt, axum and xitca-web, with
  instructions per request from cachegrind. Its latest results are in
  [../RESULTS.md](../RESULTS.md).

Deviations from the TechEmpower rules: no database (fortunes are in memory), no `Server` header.
