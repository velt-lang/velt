# Benchmark showcase: Velt against Rust, Node and Bun

One run of every comparison the repository has, on one machine, with one method: the numbers
for the website and the READMEs. It runs the programs of the existing suites under `bench/` (it
does not copy them) and adds Bun wherever a Node program exists, since Bun runs the same
`.js`/`.ts` file. Results: `results/<date>/` (`results.json`, a Markdown table per category,
`README.md` with the machine, the versions and every benchmark where Velt is not the fastest).

```sh
bench/showcase/run.sh                # everything, best of 5 (refuses on a busy machine)
bench/showcase/run.sh --smoke        # tiny sizes, one run of everything: does it all work?
bench/showcase/run.sh --categories http,websocket --runs 3
bench/showcase/report.py results/<date>   # re-render the tables from results.json
```

`run.sh --help` lists the options. On the Linux builder, from a worktree of the commit to
measure, with the machine otherwise idle:

```sh
export CARGO_TARGET_DIR=~/velt-build/showcase PATH=$HOME/.bun/bin:$HOME/velt-build/showcase/tools/bin:$PATH
nohup bench/showcase/run.sh > ~/velt-build/showcase/run.log 2>&1 &
```

It takes about 3.5–4 hours there (12 threads): about 20 minutes of builds (Velt in release,
fourteen Cargo projects, four npm installs), then the categories (estimates for the full run, in
minutes: startup 1, cpu 6, core 30, benchmarks-game 35, regex 2, async 50 (the two
`timer_await` programs take 23–44 s a run, `concurrent_map` about 50 s), json 12, tsx 5,
sqlite 5, compile 17, http 36 (with the Velt one-thread rows), websocket 5). It needs about
10 GB of disk for the build directory.

## Fairness rules

1. **The same algorithm in every language.** Every program is the existing port in its suite,
   and every implementation's output is checked against the reference (byte for byte, or the
   same JSON value, or the same checksums) in an untimed run before it is timed. An
   implementation whose output differs is shown as n/a with the reason, never timed.
2. **The same build strength.** Velt: `velt build --release`: Velt's optimizer, then LLVM
   `-O3` on the whole program (the program and the std code it uses are one LLVM module), for
   x86-64-v2 (Velt's default CPU level on x86_64). Rust gets the equivalent: every
   `cargo build --release --locked` runs with `opt-level = 3`, `codegen-units = 1`,
   `lto = "fat"` (environment overrides of each suite's profile) and
   `-C target-cpu=x86-64-v2`, and every single-file program is built with
   `rustc -C opt-level=3 -C codegen-units=1 -C lto=fat -C target-cpu=x86-64-v2` (not `-O`,
   which is opt-level 2). Where a suite's Rust baseline is built for the machine's CPU (the
   Benchmarks Game, `concurrent_map`, `btree`, `atomics`, as the published programs are), it is
   `-C target-cpu=native` and Velt gets `--cpu native`. Node and Bun run without flags (React
   with `NODE_ENV=production`). Velt's runtime library (`velt_rt`, the Rust part: tokio, hyper,
   the allocator) is linked prebuilt, built by `cargo build --release` with its own profile (thin
   LTO, baseline x86-64); the Velt servers do not use its hyper (below), and Rust's fat LTO over
   axum, hyper and tokio is if anything the stronger build.
3. **No warm-up tricks.** Every process is timed from start to exit (start-up, JIT and file
   reading included), except where a suite times its own loop for every language alike (the
   SQLite workloads, the fetch scenarios, `concurrent_map`: the program prints its own time).
   The untimed checking run comes before the timed runs for every implementation alike (it
   also brings the input files into the page cache). Servers all get the same warm-up load
   (3 s per route) before each measured run.
4. **Interleaved, best of N.** Each round runs every implementation once, and the order rotates
   by one each round, so a change of load on the machine hits all of them alike. The tables
   show the best wall time of N (default 5) and the median; CPU time is the smallest user + sys,
   peak RSS the largest of the N runs. The run refuses to start when the 1-minute load average
   is over 2 (`--force` overrides), and after the build phase it waits for the load average to
   fall back to 2 or less before the first timed run (at most 15 minutes, `run.sh
   --settle-timeout`; then it stops with a message, or goes on with `--force`). The results
   record that wait and the load average at the start of every category. It pins the CPU
   frequency governor to `performance` when it may, otherwise it records the governor as it is.
5. **Like against like.** Every implementation states how many threads it computes on, in its
   benchmark definition (`threads=ST` or `threads=MT` in `suites.py`, `core.py`, `servers.py`;
   the Benchmarks Game files are listed one by one in `suites.GAME_THREADS`, and a file missing
   there is an error, never guessed from its name). Single-threaded: a plain program, `_st`,
   `Velt 1 thread`, one Node or Bun process. Multi-threaded: `_mt`, the Rust programs on rayon
   or threads, Node's `worker_threads` programs, tokio's multi-thread runtime, `node:cluster`,
   Bun `reusePort`, Velt with `VELT_THREADS` at the server cores. The tables' *threads* column
   shows it, and *vs Velt* compares each row with Velt's first implementation of the same kind
   (named when there are several), or says "no Velt multi-threaded version" (or
   single-threaded) when there is none. The servers and the fetch client have a Velt row on one
   thread for that reason.
6. **Every result is reported, losses included.** Every implementation of every benchmark that
   runs is in the tables, and the results' `README.md` lists every benchmark where another
   implementation beat Velt's implementation of the same kind, and every kind that has no Velt
   implementation. Variants (`main_mt`, `main_opt`, `_st`, `Velt 1 thread`, `node:cluster`) are
   labelled as such, beside the default implementation, never in its place.

Measurement: wall time, CPU time and peak RSS come from `measure.c`, a small helper built at
the start of a run: it forks and execs the program, times it with the monotonic clock and
takes `ru_utime`, `ru_stime` and `ru_maxrss` from `wait4`, the numbers GNU `time -v` prints,
without needing it installed (the builder has no `/usr/bin/time`). The harness cannot take them
from its own `wait4`: Python starts a child with `vfork`, and Linux starts the child's
`ru_maxrss` at the peak RSS of the address space its `exec` replaced, which is the harness's own
(hundreds of MB after reading a large output), so every program smaller than the harness
reported the harness's size. The run checks the helper first on two programs (`true` must
report under 16 MB while the harness holds 64 MB more, a program touching 128 MB over 120 MB),
and without a working helper the peak RSS cells stay empty. Server CPU and RSS come from
`/proc` over the server's whole process tree (all `node:cluster` workers, all Bun `reusePort`
processes; `VmHWM`, each process's own peak).

## Categories

| category | benchmarks (suite) | Velt | Rust | Node | Bun |
|---|---|:-:|:-:|:-:|:-:|
| `startup` | hello world: start-up time (4 × N rounds of one run each instead of N) and executable size (`programs/hello`) | ✓ | ✓ | ✓ | ✓ |
| `cpu` | 14 micro-benchmarks: classes, closures (4), fib, floats, hashmap, int32, loops, nbody, shapes, sort, strings (`bench/*.vlt`, `bench/rust`, `bench/node`) | ✓ | ✓ | ✓ | ✓ |
| `benchmarks-game` | binary-trees, fannkuch-redux, fasta, k-nucleotide, mandelbrot, n-body, pidigits, reverse-complement, spectral-norm at the official sizes, every variant in the suite (`bench/benchmarks-game`) | ✓ | ✓ | ✓ | ✓ |
| `regex` | regex-redux on `fasta 5000000` (`bench/benchmarks-game/regex-redux`) | ✓ | ✓ (regex crate) | ✓ | ✓ |
| `async` | 14 async programs: await chains, fan-out, channels, spawn, timers (`bench/async`; Rust on tokio multi-thread and current-thread, Velt on all cores and on 1 thread) | ✓ | ✓ | ✓ | ✓ |
| `async` | the fetch client: seq, conc, big, json, gzip against one hyper server (`bench/http/fetch`) | ✓ | ✓ (reqwest) | ✓ (undici) | ✓ |
| `async` | `concurrent_map`: velt:collections ConcurrentMap against bbtree's `BBTreeMap` (`bench/concurrent_map`) | ✓ | ✓ | | |
| `json` | parse_typed, parse_union, parse_value, stringify, edit, navigate (`bench/json`) | ✓ | ✓ (serde_json) | ✓ | ✓ |
| `json` | canada, citm_catalog, twitter, 50 rounds each (`bench/json/corpora`): parse to a value (Velt `JSON.parseValue`), write a parsed value back (Velt `JsonValue.stringify()`) | ✓ | ✓ (serde_json, sonic-rs, simd-json) | ✓ | ✓ |
| `json` | the same corpora typed (`json/typed.vlt`): `JSON.parse<T>` into structs with the fields a program reads, and `JSON.stringify` of the typed value | ✓ | ✓ (serde derive on serde_json, sonic-rs, simd-json: `json/rust`) | ✓ (`JSON.parse` + the same field reads) | ✓ |
| `tsx` | server-side rendering of one component tree (`tsx/_page.vlt`, `tsx/page.mjs`): a 10-row page × 20 000 and a 1000-row page × 500, renders per second; the HTML compared byte for byte once | ✓ (velt:jsx) | n/a | ✓ (React `renderToString`, Preact `preact-render-to-string`) | ✓ (the same) |
| `core` | collections, strings, language features: `bench/typical` (19 TypeScript programs, each with a Rust port), `bench/iter`, `bench/strings_utf16`, `bench/key_order`, `bench/any_ops`, `bench/any_text`, `bench/typed_array`, `bench/structs`, `bench/btree`, `bench/atomics`, `bench/stdport` (below) | ✓ | most | most | most |
| `sqlite` | insert in a transaction, point select, range select, autocommit insert (`bench/db`, file database, WAL) | ✓ | ✓ (rusqlite) | ✓ (better-sqlite3) | ✓ (bun:sqlite, `bun/sqlite.ts`) |
| `sqlite` | rows to JSON: the feed and post queries of `bench/feed` (`bench/sqlite`) | ✓ | ✓ (rusqlite + serde_json) | | |
| `compile` | hello world, nbody, the TechEmpower server (`velt/techempower_server.vlt` against axum): Velt debug and release (build cache off) beside `rustc` and `cargo build` (clean, with dependencies; and after touching `main.rs`) | ✓ | ✓ | | |
| `http` | `/plaintext`, `/json`, `/fortunes` (sorted, escaped HTML table) with oha (`bench/http/techempower`; Velt in `velt/techempower_server.vlt`, Bun.serve in `bun/techempower.ts`) | ✓ (velt:http/server, written in Velt) | ✓ (axum) | ✓ (node:http, 1 process and node:cluster) | ✓ (Bun.serve, 1 process and `reusePort` × cores) |
| `http` | `TSX page /small`, `/large`: the `tsx` page rendered per request (`tsx/server.vlt` on velt:http/server, `tsx/server.mjs`) | ✓ (velt:http/server) | n/a | ✓ (node:http + React or Preact, 1 process and node:cluster) | ✓ (Bun.serve + React or Preact, 1 process and `reusePort` × cores) |
| `websocket` | the chat app under steady load: 2000 clients, 20 rooms, 1000 messages/s (`bench/chat`, Velt in `velt/chat_server.vlt`, load from `chat/load`) | ✓ (velt:http/server + velt:websocket; server cores, and 1 thread) | | ✓ (ws, 1 process) | ✓ (1 process) |
| memory | peak RSS of every benchmark above, one table (`memory.md`), a row per thread kind: each language's single-threaded implementation (its `_st` one where it has several) beside the others', and its multi-threaded one beside theirs | | | | |

The JSON rows say which Velt API they use; all of them run the JSON engine written in Velt
(std/prelude/json and velt:json: the scanner and reader, the writer, the tape-backed
`JsonValue`), and none goes through a Rust fallback. React runs with `NODE_ENV=production`, as
it is deployed; the npm versions are pinned in `tsx/package.json` (and its lockfile).

### Core features

| area | benchmark (suite) | Velt | Rust | Node | Bun |
|---|---|:-:|:-:|:-:|:-:|
| collections | `Map`/`Set` counting, records, objects, sort with a comparator, statistics (`typical`: keys, wordcount, record, objects, sortcmp, stats) | ✓ | ✓ (HashMap, Vec) | ✓ | ✓ |
| collections | hashmap, sort (`cpu` above) | ✓ | ✓ | ✓ | ✓ |
| collections | `BTreeMap`: inserts, lookups, ranges, removal (`btree`; the program times each workload) | ✓ | ✓ (std `BTreeMap`, bbtree) | n/a: no built-in sorted map | n/a |
| collections | typed arrays and byte loops: u8 read-modify-write, f64 dot and axpy, i32 sum, xor, nested (`typed_array/loops.ts`) | ✓ | n/a: no Rust program in the suite | ✓ | ✓ |
| collections | object key order on records and index keys (`key_order`) | ✓ | n/a | ✓ | ✓ |
| concurrency | `Atomic<T>`: counters, CAS loops, latches across threads (`atomics`) | ✓ | ✓ (`std::sync::atomic`) | n/a: no JS program (would need workers and a SharedArrayBuffer) | n/a |
| concurrency | tasks, channels, fan-out, timers (`async` above); `ConcurrentMap` against bbtree's concurrent map (`async` above) | ✓ | ✓ | ✓ (tasks only) | ✓ (tasks only) |
| strings | string building, split/join, tokenizing (`typical`: strings, tokenize); non-ASCII strings: append, charCodeAt, CSV, JSON, sort (`strings_utf16`) | ✓ | ✓ (typical) | ✓ | ✓ |
| strings | number formatting and parsing (int and float to string, `toFixed`, `parseFloat`), UTF-8 decode and validation, base64, hex, SHA-256, URL parsing, big integers, gunzip, Brotli (`stdport`: Velt std against the Rust crate for the same job, one thread) | ✓ | ✓ (itoa, ryu, url, dashu-int, sha2, base64, ...) | n/a | n/a |
| language | closures, callbacks, classes and virtual dispatch, error handling, generics over trees and graphs, numeric loops (`typical`: callbacks, chains, consume, dispatch, errors, fib, graph, grid, numloop, trees, vec2; classes, closures, shapes in `cpu`) | ✓ | ✓ | ✓ | ✓ |
| language | iterators and generators, sync and async, spread (`iter`) | ✓ | n/a | ✓ | ✓ |
| language | `any` operations: arithmetic, equality, `in`, casts; `any` over classes, maps and objects (`any_ops`, `any_text`) | ✓ | n/a: no dynamic typing | ✓ | ✓ |
| language | value types: `struct` and `class` against a Rust `Copy` struct (`structs`: mapkey, particles, points, vecmath) | ✓ (both) | ✓ | ✓ (class) | ✓ (class) |

Compile times are labelled for what they are: Velt compiles the program and the std it uses
from source with the build cache off, against a runtime library that is prebuilt (as Rust's std
is). A clean `cargo build` of the axum server compiles axum, hyper and tokio as well; the row
with `main.rs` touched is the edit-and-rebuild time.

Servers run pinned to whole physical cores and the load generator to the others (`taskset`):
half of the physical cores each (on the 6-core builder: 3 cores, 6 threads, for the servers and
3 for oha), so the load generator can keep up with the fastest servers; `SERVER_CPUS` and
`LOAD_CPUS` override, as taskset lists. Each timed run records oha's own CPU time, and a row
where it used 90% or more of its CPUs is marked **load-bound** (its req/s may be the load
generator's limit, not the server's); a loss line in the results' README says
"(load-bound)" when either row was. Every server table, the TechEmpower routes and the TSX
pages alike, has each JavaScript server twice: as one process (one core) and on every server
core (`node:cluster` with one worker per core; Bun with `reusePort` and one process per core).
The Velt servers are written in Velt: `velt:http/server`, the HTTP/1.1 server on std/net
(`std/http/server.vlt`), and for the chat `velt:websocket`'s `acceptWebSocket` on it, not
`velt:http`, whose `serve` is hyper in the Rust runtime. `velt/techempower_server.vlt` has the
routes of `bench/http/techempower/server.vlt`, `velt/chat_server.vlt` is
`bench/chat/velt/server.vlt` with the server swapped (both suites' own programs stay on
`velt:http`), and `tsx/server.vlt` serves the TSX page. They use the server as a program would:
`serve` with its defaults (one accept loop, one task per connection on the runtime's threads,
keep-alive and pipelining as for any client), no options the other servers do not get. Velt
(`VELT_THREADS` set to the server CPUs) and axum use every server core, and Velt runs a second
time on one thread (`VELT_THREADS=1`), the row the one-process servers are compared with.
One worker thread, that is: the runtime also has its monitor thread, and may start a spare
worker while one is blocked; every server run records the server's peak thread count (all its
processes, from `/proc/<pid>/status`) as `server_peak_threads` in results.json.
Before any timing, every server is started once
and every route fetched: status 200, and the body and content-type of the reference server's
response (axum for the TechEmpower routes, the page `tsx/render.vlt` prints for the TSX page,
which the `tsx` category compares with React's and Preact's byte for byte). A server that fails
the check, or answers anything but 200 in a timed run, gets no req/s.

Results contain no host name, user name or absolute path: the host is a label
(`SHOWCASE_HOST_LABEL`, default "benchmark host") and paths are written relative to the
repository (`<repo>`), the build directory (`$BUILD`) and the home directory (`~`).
`results/` is gitignored; a run's results are committed deliberately (`git add -f`), after
review. A benchmark, category or server that raises an error is recorded as failed in
`results.json` and the run goes on; the tables are written at the end in any case.
`results.json` also records the installed npm package versions (react, react-dom, preact,
preact-render-to-string, better-sqlite3, ws), the SQLite version that better-sqlite3, bun:sqlite
and velt:sqlite report (`sqlite_version()`), and for rusqlite the version of the
`libsqlite3-sys` crate, whose release fixes the SQLite it bundles.
`test_report.py` and `test_measure.py` (run by `run.sh` first) test the list of losses and the
like-for-like pairing on made-up results, that every Benchmarks Game file is classified, the
measure helper's peak RSS (two very different programs must report different peaks, whatever
the harness's own size), and the wait for a quiet machine.

## Not covered

- **Postgres and Redis** (`bench/db`): they need a server; `bench/db/run.sh` with
  `BENCH_PG_URL` / `BENCH_REDIS_URL` runs them.
- **The feed API** (`bench/feed`): it needs the challenge repository's seed database and a Rust
  submission's binary (`bench/feed/README.md`).
- **bench/regex** (`literal.vlt`, `units.vlt`): Velt-only instruction-count guards with no
  counterpart in the other languages; regex-redux is the cross-language regex benchmark.
- **Go** programs exist in the Benchmarks Game and chat suites; this showcase compares with
  Rust, Node and Bun only (`bench/benchmarks-game/run.sh` includes Go).
- **Rust for the chat server, `bench/sqlite` for Node and Bun, Rust for the `iter`, `any`,
  `key_order` and typed-array programs, JavaScript for `btree`, `atomics` and `stdport`**: there
  is no such program in the suites, and none was written for this; those cells are n/a with the
  reason.
- **Threads against Node `worker_threads` and Bun workers outside the Benchmarks Game, and
  `ConcurrentMap` against Rust's DashMap**: only the Benchmarks Game has worker-thread
  programs, and `concurrent_map` compares with bbtree's concurrent map only.
- **Clustered Node and Bun chat servers**: the Bun and Node chat servers keep a room's members
  in the process (Bun's pub/sub, ws's client sets), so several processes behind `reusePort` or
  `node:cluster` would each deliver a room's messages only to their own clients; a fair
  multi-process version needs shared state (a broker) that the suite does not have. Their rows
  say "single process; rooms need shared state" and are compared with Velt on one thread;
  Velt on the server cores has no counterpart and is not counted as a win.
- **Rust templating for the TSX rows** (askama, maud): templating is not JSX, so the Rust cell is
  n/a rather than a different kind of program.
- **bench/array_view, bench/shared, bench/simd, bench/proxy and the other instruction-count
  suites**: Velt-only A/B guards (`shared` measures Velt's `shared<T>` and locks, with no
  counterpart program).
- **Cranelift release builds**: the suites' own harnesses (`bench/run.sh`,
  `bench/benchmarks-game/run.sh`) compare the two Velt backends.
