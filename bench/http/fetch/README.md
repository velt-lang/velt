# fetch client benchmark

Velt's global `fetch` against Node's (undici) and Rust's reqwest, all calling one local hyper
server (`rust/src/bin/server.rs`), in four scenarios:

| Scenario | What the client does |
|---|---|
| `seq` | 10,000 sequential `GET /small` (13 bytes) on a keep-alive connection, reading each body |
| `conc` | 100 concurrent workers × 1,000 `GET /small` |
| `big` | one `GET /big` (100 MB), read with `bytes()` |
| `json` | 20 × `GET /json` (a 1 MB array of users), decoded into a typed array |

Run `bench/http/fetch/run.sh` (Linux, macOS; it adds the instructions each client executed
when `perf` is available) or `pwsh bench/http/fetch/run.ps1` (Windows). Both build the Rust
server and the reqwest client in a temporary target directory (`BENCH_TARGET_DIR` overrides
it), build `client.vlt` with a release `velt`, and print each client's own result with its CPU
time and peak memory.

## Results

Windows 11, 20 logical cores, on a machine shared with other builds at full load, so wall-clock
results vary by ±30% between runs; best of 2–5 interleaved runs, with the process's CPU time
(user + system) and peak RSS.

| Scenario | Velt | Node 22.22 | Rust reqwest 0.12 |
|---|---|---|---|
| `seq` | 7,440 req/s · 1.1 s CPU · 8 MB | 2,024 req/s · 6.4 s · 113 MB | 4,582 req/s · 2.2 s · 6 MB |
| `conc` | 46,221 req/s · 11 s · 24 MB | 2,314 req/s · 45 s · 141 MB | 43,741 req/s · 11.5 s · 15 MB |
| `big` | 598 MB/s · 0.2 s · 110 MB | 106 MB/s · 1.0 s · 281 MB | 259 MB/s · 0.2 s · 112 MB |
| `json` | 7.0 ms per 1 MB · 0.1 s · 26 MB | 31 ms · 0.8 s · 92 MB | 17 ms (serde) · 0.2 s · 9 MB |

Before the global `fetch` (#577), `big` peaked at 318 MB: the body was copied twice. On this
machine the per-request CPU of `seq` is 0.97–1.6 s for 10,000 requests both before and after;
a difference smaller than that spread needs the instruction counts of `run.sh` on a quiet Linux
machine.
