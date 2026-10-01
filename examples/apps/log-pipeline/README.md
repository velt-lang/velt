# log-pipeline

Stream-processes a large access log: chunked file reads (`std/fs_stream`), a regex per line
(`std/regex`), `Map` aggregation, sorting and a text report — plus the same program in Node
(`node/pipeline.mjs`, idiomatic JS: `readline`, regex literals, `Map`, `sort`) for a performance
comparison on the same input. Both print byte-identical reports; the generators write
byte-identical logs.

```sh
velt build --release
./target/velt/log-pipeline gen access.log --lines 1000000     # ~93 MB, deterministic (--seed)
./target/velt/log-pipeline report access.log [--timing]
node node/pipeline.mjs report access.log
./bench.sh [lines]                                            # both, 3 runs each, checks outputs
```

The report: totals, status classes, the 10 busiest routes (ids folded to `:id`) with error rate
and p50/p95/p99 latency, the 5 busiest clients and the 3 hours with the most 5xx responses.

## Performance (Apple M-series, macOS, 1,000,000 lines / 93 MB, median of 3)

| | Velt (`--release`, LLVM) | Node 24 |
|---|---|---|
| generate: user CPU | 1.07 s | 1.25 s |
| report: user CPU | 2.49 s | 2.30 s |
| report: wall | 2.80 s | 2.73 s |
| report: peak RSS | 39 MB | 225 MB |

Parity on CPU, ~6x less memory. The first, TS-natural version read the file with
`FileReader.readLine()` (like `for await (const line of rl)`) and took ~5x Node's CPU time: each
`readLine` is a runtime round trip (~10 µs). `src/lines.vlt` reads 1 MiB chunks and splits them
instead (46x faster reading). Of the remaining time, about 40% is regex capture extraction
(`RegExp.exec` with 7 groups; `test` alone is 4x cheaper).

| File | What |
|---|---|
| `src/generate.vlt` | deterministic LCG log generator |
| `src/lines.vlt` | `LineSplitter`: chunks → lines |
| `src/parse.vlt` | `LineParser`: regex → `Entry`, route normalization |
| `src/stats.vlt` | `Stats`: per-route / client / hour aggregation |
| `src/report.vlt` | percentiles, formatting, sorting |
| `src/analyze.vlt` | the streaming loop |
| `tests/*.test.vlt` | `velt test` |
| `demo.vlt` / `demo.out` | 5000-line golden (`cargo test -p veltc --test golden`); Node prints the same |
