# Benchmarks

The repository compares Velt with Rust, Node and Bun on the same programs, and the numbers
published for Velt come from one script: `bench/showcase/run.sh`. It runs the existing suites
under [`bench/`](../../bench/) (micro-benchmarks, core language and library features, the
Benchmarks Game, async, JSON, TSX rendering against React and Preact, regex, SQLite, HTTP and
WebSocket servers, start-up, compile time and memory) in one pass, with one method, and writes
the results as JSON and as Markdown tables.

```sh
bench/showcase/run.sh --smoke     # tiny sizes, one run of everything: checks that it all works
bench/showcase/run.sh             # the full run, best of 5 (about 3.5-4 hours on 12 threads)
bench/showcase/run.sh --categories http,websocket --runs 3
```

It runs on Linux and needs cargo, clang, python3, node, bun and, for the HTTP servers, `oha`.
The full list of benchmarks, which languages each has and what is not covered are in
[`bench/showcase/README.md`](../../bench/showcase/README.md).

## How the comparison is kept fair

- **The same algorithm in every language**, and every implementation's output is checked
  against the reference before it is timed; an implementation with a different output is
  reported as n/a, not timed.
- **The same build strength**: `velt build --release` is LLVM `-O3` on the whole program for
  x86-64-v2, so Rust is built with `opt-level = 3`, one codegen unit, fat LTO and
  `-C target-cpu=x86-64-v2` (`cargo build --release --locked`, and `rustc` with those flags for
  single-file programs). Where a Rust baseline is built for the machine's CPU, Velt is too. Node
  and Bun run without flags.
- **Checked before timed**: servers too, by status, body and content-type of every route.
- **No warm-up that favours one side**: whole processes are timed from start to exit unless a
  suite times its own loop in every language alike, and every server gets the same warm-up.
- **Interleaved runs, best of N**: each round runs every implementation once in a rotating
  order, so changes of load hit all of them alike. The script refuses to start on a busy
  machine (load average over 2) and records the CPU, kernel, governor and every toolchain's
  version beside the numbers.
- **A quiet machine when timing starts**: building everything leaves the load average high, so
  after the build the script waits for it to fall back to 2 or less (at most 15 minutes,
  `--settle-timeout`; then it stops, unless `--force`). The wait and the load average at the
  start of every category are recorded with the results.
- **Like against like**: every implementation is marked single-threaded (one thread, one
  process) or multi-threaded (threads, worker threads, several processes) in its benchmark's
  definition, and the tables show it in a *threads* column. Each row is compared with Velt's
  implementation of the same kind: Rust on all cores with Velt's multi-threaded program, a
  single-threaded Rust, Node or Bun program with Velt's single-threaded one. Where Velt has no
  implementation of a kind, the row says so ("no Velt multi-threaded version") instead of
  comparing across kinds. The servers and the fetch client also run Velt on one thread, the
  counterpart of one Node or Bun process. The memory table pairs the same way.
- **Peak RSS of each program itself**: a small helper (`bench/showcase/measure.c`) forks, runs
  and waits for every timed program and reports its wall time, CPU time and peak RSS
  (`wait4`'s `ru_maxrss`, what GNU `time -v` prints). The harness does not use its own `wait4`
  for this: Python starts a program with `vfork`, and Linux then starts the program's peak RSS
  at the harness's own, so every program smaller than the harness would report the harness's
  size. Each run first checks the helper on two programs of very different size and leaves
  the memory cells empty if they do not report different peaks. Servers' peak RSS comes from
  `/proc` (`VmHWM`) over all their processes.
- **Velt's servers are written in Velt**: the HTTP, TSX page and WebSocket chat rows run on
  the `http-server` package, the HTTP server written in Velt on std/net (with
  `velt/net/websocket` for the chat), with its default options.
- **What could not be made equal is labelled**: the Bun and Node chat servers keep each room's
  members in the process, so they cannot run as several processes without shared state. They
  run as one process each, labelled "single process; rooms need shared state", and are
  compared with Velt on one thread; Velt on every core has no counterpart there and is not
  counted as a win.
- **Every result is published**, including the benchmarks where Velt is slower: the results
  list each benchmark where another implementation beat Velt's implementation of the same
  kind, and each kind Velt has no implementation of.

Compile times are labelled for what they measure: Velt compiles the program and the parts of
std it uses with the build cache off, against a prebuilt runtime (as Rust's std is prebuilt),
beside `rustc`, a clean `cargo build` that also compiles the dependencies, and a rebuild after
an edit.
