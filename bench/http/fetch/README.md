# fetch client benchmark

Velt's global `fetch` against Node's (undici) and Rust's reqwest, all calling one local hyper
server (`rust/src/bin/server.rs`), in five scenarios:

| Scenario | What the client does |
|---|---|
| `seq` | 10,000 sequential `GET /small` (13 bytes) on a keep-alive connection, reading each body |
| `conc` | 100 concurrent workers × 1,000 `GET /small` |
| `big` | one `GET /big` (100 MB), read with `bytes()` |
| `json` | 20 × `GET /json` (a 1 MB array of users), decoded into a typed array |
| `gzip` | as `json`, from `GET /json-gzip`: the same 1 MB gzip-compressed (`content-encoding: gzip`), so the client decodes it |

Run `bench/http/fetch/run.sh` (Linux, macOS; it adds the instructions each client executed
when `perf` is available) or `pwsh bench/http/fetch/run.ps1` (Windows). Both build the Rust
server and the reqwest client in a temporary target directory (`BENCH_TARGET_DIR` overrides
it), build `client.velt` with a release `velt`, and print each client's own result with its CPU
time and peak memory. `COUNT=valgrind bench/http/fetch/run.sh` runs each client once under
cachegrind instead and prints the instructions it executed (`I refs`). It needs no PMU, so it
works in WSL and VMs where `perf` can't count, and the count varies far less between runs than
times on a busy machine.

The nightly benchmark guard (`bench/nightly.sh`) counts `bench/fetch/loopback.velt` instead: the
client against a Velt server in the same process (small, JSON and gzip bodies, sequential), which
needs no Rust build and whose count repeats to within about 0.2%.

The reqwest client is built with reqwest's `gzip` feature (for `gzip`), so it sends
`accept-encoding: gzip` in every scenario; Velt and Node send `accept-encoding: gzip, deflate`.
The server ignores it except on `/json-gzip`.

## Results

The client is Velt code (`std/net/http/fetch/client/`) on `velt/net`, with gzip and Brotli
decoded by `velt/compress`. Instructions each client executed (cachegrind's `I refs`,
millions), Linux x86-64 container, 4 cores, 2026-10-28, commit 692592c. Node is not counted:
V8's JIT under cachegrind says little about its speed.

| Scenario | Velt | reqwest 0.12 | Velt / reqwest |
|---|---:|---:|---:|
| `seq` | 164.3 | 645.8 | 0.25 |
| `conc` | 1,474 | 5,957 | 0.25 |
| `big` | 1.8 | 107.6 | 0.02 |
| `json` | 659.5 | 783.3 | 0.84 |
| `gzip` | 832.2 | 977.1 | 0.85 |

Wall clock under cachegrind follows the same order (`seq`: 4,041 req/s against reqwest's 919;
`big`: 365 MB/s). Where it comes from:

- A request is written into one array with its small body and leaves in one write; the response
  head is parsed in place in the connection's read buffer, and its headers become strings only
  when read. A body that arrives with its head (most small responses) gives the connection back
  to the pool at once, and `text()` decodes it from the read buffer.
- A body of known length is read from the socket straight into an array of its size: `big`
  copies nothing.
- `text()` validates UTF-8 before decoding (`Buffer.utf8Slice`); invalid UTF-8 becomes U+FFFD,
  as in JS.
- gzip: `velt/compress` decodes the 1 MB in 0.65 of `flate2`'s instructions
  (`bench/stdport` `gunzip`), its CRC-32 folded with carry-less multiplication (PCLMULQDQ /
  PMULL through `velt/simd`'s `u64x2.clmulLow` and related methods), as `flate2` does.
- These scenarios use plain HTTP/1.1; over TLS the client also speaks HTTP/2.
- What a request costs beyond a bare HTTP exchange is what the Fetch API asks for: the three
  request headers Node also sends (`accept`, `user-agent`, `accept-encoding`), a response that
  resolves with its head and receives its body in a second step, and the `Response` object.
