# Computer Language Benchmarks Game — Velt vs Rust, Go, Node, Bun

Ports of the [Benchmarks Game](https://benchmarksgame-team.pages.debian.net/benchmarksgame/)
programs. Run everything with `bench/benchmarks-game/run.sh` (see `--help`); results are in
`RESULTS.md`, root causes of Velt gaps in `bench/FINDINGS.md`.

## Layout

```
<program>/
  main.vlt        idiomatic Velt, written the way a TypeScript developer would
  main_mt.vlt     multi-threaded Velt (spawn + Promise.all), where the official fastest are parallel
  main_opt.vlt    optimized Velt, only when it teaches something about the compiler
  main.go          fastest single-file published Go (credited in its header)
  main.js          fastest single-file published Node program (Bun runs the same source)
  main_st.go/.js   fastest published single-threaded Go / Node, when main.go / main.js is parallel
  bench.conf       shell variables read by run.sh (below)
  expected-quick.txt   expected output for QUICK_N (the official output file where one exists)
  NOTES.md         port notes: sources/credits, deviations, Velt gaps and their root causes
rust/              one Cargo project: src/bin/<program>.rs (the fastest published program) and
                   <program>_st.rs (the single-thread reference), plus extra baselines
                   (binary-trees_box, pidigits_limbs); deps rayon, regex, bumpalo, mimalloc, rug
```

`bench.conf`:

```sh
N=50000000          # official benchmark argument
QUICK_N=1000        # small argument for correctness runs (matches expected-quick.txt)
STDIN=none          # none | fasta  (fasta: stdin is the output of `fasta N`, or `fasta QUICK_N` for quick runs)
```

Programs reading stdin (`STDIN=fasta`) get the fasta output: the runner generates it once with the
Rust fasta at the N in `bench.conf` (25,000,000; 5,000,000 for regex-redux) and at QUICK_N (1000: the official small input files are exactly `fasta 1000`).

Setup on macOS: `brew install go bun gmp` (GMP for the Rust `rug` and Go pidigits programs); Rust
and Apple clang from the usual toolchains.

Correctness: quick runs compare byte-for-byte with `expected-quick.txt`; full runs compare every
implementation byte-for-byte with the Rust output (whose quick output matches the official one).
