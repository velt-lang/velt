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
it), build `client.vlt` with a release `velt`, and print each client's own result with its CPU
time and peak memory. `COUNT=valgrind bench/http/fetch/run.sh` runs each client once under
cachegrind instead and prints the instructions it executed (`I refs`). It needs no PMU, so it
works in WSL and VMs where `perf` can't count, and the count varies far less between runs than
times on a busy machine.

The reqwest client is built with reqwest's `gzip` feature (for `gzip`), so it sends
`accept-encoding: gzip` in every scenario; Velt and Node send `accept-encoding: gzip, deflate`.
The server ignores it except on `/json-gzip`.

## Results

Instructions each client executed in user space (`COUNT=valgrind`: cachegrind's `I refs`, in
millions, the mean of 2 runs, which differed by less than 0.2%), Ubuntu 20.04 in WSL 2 on a
shared Windows machine. Wall-clock times on that machine vary by ±30% between runs, so they
are not shown. Node is not counted: V8's JIT under cachegrind says little about its speed.

| Scenario | `main` before #577 | `main` (9f81cea) | #601 / #653 | reqwest 0.12 |
|---|---|---|---|---|
| `seq` | 341.0 | 476.9 | 361.8 | 666.8 |
| `conc` | 3,110 | 4,482 | 3,342 | 5,955 |
| `big` | 121.7 | 107.8 | 107.7 | 107.9 |
| `json` | 673.4 | 805.3 | 665.5 | 787.4 |
| `gzip` | – | 1,055.6 | 915.2 | 984.0 |

The pull request for #601 and #653 took back most of what the global `fetch` added per
request (`seq`: from 13,600 instructions per request more than before #577 to 2,100 more):

- The URL: a URL already in the form WHATWG parsing would give it (`http://host:port/path`,
  a lowercase ASCII host, nothing to normalize; one pass over its bytes) is taken as it is,
  and only other URLs go through the `url` crate and IDNA (`seq` −42.6 M). The `Uri` shares
  the URL's buffer and moves into the request.
- The response: its status text, URL, `redirected` and headers are copied out of the runtime
  the first time a program reads them, not on every response (−31.1 M); a response or request
  without a body makes no body parts.
- `fetch(url)` sends without making a `Request` object.
- The request's headers: the defaults (`accept`, `user-agent`, `accept-encoding`) are copied
  from a map made once, and an `http:` request gets its `host` from the URL instead of hyper's
  `format!`; a request without headers of its own copies none for a redirect, and `location` and
  `content-length` are only looked up when needed.
- `text()` (#653): valid UTF-8 is only validated (ASCII only scanned) and copied once, where
  `String::from_utf8_lossy` took about 7 instructions per byte; invalid UTF-8 still becomes
  U+FFFD (`json` −140 M, `gzip` −140 M). Header values take the same path.

What remains of the 2,100 per request against `main` before #577 is what the Fetch API asks
for: the three request headers Node sends (`accept`, `user-agent` and #594's
`accept-encoding`: hashing, writing and the server parsing them), a response that resolves
with its head and receives its body in a second step (a second runtime future and registry
lookup), and the `Response` object.

### History

The Velt clients of the table below come from five compilers: `main` before the global `fetch`
(080387d, calling `fetch` from `velt:http`), the global `fetch` (#577, a0b9bd5), the base of
#594 (d52bf62, #577 plus #557), the decoding of #594 (891150d) and this benchmark's pull
request (#600). `seq` and `conc` were counted again after both were merged with `main`: those
two rows show `main` at 4b459dc as #594's base, #594 at 683cdbe (which sizes the request's
header map for its three default headers) and #600 on top of it.

| Scenario | `main` | #577 | #594's base | #594 | #600 | reqwest 0.12 |
|---|---|---|---|---|---|---|
| `seq` | 341.0 | 463.8 | 463.5 | 477.9 | 477.2 | 666.8 |
| `conc` | 3,110 | 4,350 | 4,347 | 4,493 | 4,482 | 5,955 |
| `big` | 121.7 | 107.7 | 107.6 | 107.7 | 107.8 | 107.9 |
| `json` | 673.4 | 805.4 | 805.5 | 805.4 | 805.7 | 787.4 |
| `gzip` | – | – | – | 1,055.4 | 1,055.4 | 984.0 |

- `seq` and `conc` (#577: +36% and +40%, about 12,300 instructions per request): the global
  `fetch` parses the URL as WHATWG URL (`url` and `idna`, about 3,600), builds the std objects
  (`Request`, a `Headers` copy; about 1,000), makes Velt strings of the header values and the
  URL (lossy UTF-8 decoding and UTF-16 lengths: about 1,700), adds `accept` and `user-agent`,
  and copies the status, URL and headers when the head arrives. #601 tracks this.
- `seq` and `conc` (#594: +3.1% and +3.3%, about 1,450 instructions per request): sending
  `accept-encoding`, deciding how to decode the response (`content-encoding`, HEAD and bodiless
  statuses), and the `Reader` that yields chunks (about 150 more than the plain read). The
  request's header map has room for `accept`, `user-agent` and `accept-encoding`; at 891150d it
  grew for the third, and the request cost about 1,910.
- `big` (#577: −11.5%): the body is received into one buffer sized from `content-length` and
  copied once, where `main` copied it twice (peak memory on Windows: 318 MB before, 110 MB after).
- `json` (#577: +20%, 6.6 million per 1 MB response): `text()` decodes with
  `String::from_utf8_lossy` (invalid UTF-8 becomes U+FFFD, as in JS), about 7 instructions per
  byte, where `main` validated with `str::from_utf8` (under 0.5 per byte) and failed on invalid
  UTF-8.
- `gzip`: before #594 the body was not decoded, so the JSON did not parse. Decoding the 1 MB
  takes about 12.5 million instructions in Velt and 9.8 million in reqwest (both through
  `flate2`).
- #600 against #594: `seq` −0.15% and `conc` −0.24%, the others within 0.1%. Its header move
  (`send.rs`) applies only to requests that don't follow redirects, and these do.
