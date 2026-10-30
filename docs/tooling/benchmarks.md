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
- **Release builds with default flags**: `velt build --release`, `cargo build --release`, and
  Node and Bun without flags. Where a Rust baseline is built for the machine's CPU, Velt is
  too.
- **No warm-up that favours one side**: whole processes are timed from start to exit unless a
  suite times its own loop in every language alike, and every server gets the same warm-up.
- **Interleaved runs, best of N**: each round runs every implementation once in a rotating
  order, so changes of load hit all of them alike. The script refuses to start on a busy
  machine (load average over 2) and records the CPU, kernel, governor and every toolchain's
  version beside the numbers.
- **Every result is published**, including the benchmarks where Velt is slower: the results
  list each benchmark where another implementation beat Velt.

Compile times are labelled for what they measure: Velt compiles the program and the parts of
std it uses with the build cache off, against a prebuilt runtime (as Rust's std is prebuilt),
beside `rustc`, a clean `cargo build` that also compiles the dependencies, and a rebuild after
an edit.
