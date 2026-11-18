# regex-redux: port notes

Rules: <https://benchmarksgame-team.pages.debian.net/benchmarksgame/description/regexredux.html>.
The official size for regex-redux is stdin = `fasta 5000000` (50.8 MB), not 25,000,000; quick
stdin is `fasta 1000` = the official 10 KB `regexredux-input.txt`, and `expected-quick.txt` is
the official `regexredux-output.txt`.

## Sources

| File | Program | Authors | Threads |
|---|---|---|---|
| `rust/src/bin/regex-redux.rs` | [Rust #6](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/regexredux-rust-6.html) (unchanged) | Tom Kaitchuck | rayon |
| `rust/src/bin/regex-redux_st.rs` | Rust #6 on a one-thread rayon pool | | 1 |
| `main.go` | [Go #3](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/regexredux-go-3.html) (unchanged) | Dean Becker | goroutines |
| `main.js` | [Node #3](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/regexredux-node-3.html) (unchanged) | Jesse Millikan, jose fco. gonzalez, Matthew Wilson, Roman Pletnev, Josh Goldfoot, Andrey Filatkin | 2 (1 worker) |
| `main.velt` | idiomatic Velt: `readAll()` into a string, `new RegExp(p, "g")`, `.matches(s).length`, `.replace(s, r)` | | 1 |
| `main_mt.velt` | `main.velt` with the substitution chain and each variant count in spawned tasks (like Rust #6's `rayon::join` + `par_iter`) | | 10 tasks |

Why not the fastest published programs: Rust #7 (0.78 s) binds PCRE2 (`pcre2_sys`, `libc`) and
Go #5 / #4 bind PCRE through third-party cgo modules; the bench project does not depend on
PCRE (`rust/Cargo.toml`), and Go builds single files with the standard library. Rust #6 is the
fastest Rust program that uses the `regex` crate, the engine under Velt's `RegExp`
(`velt/text`, which calls the runtime's `crates/velt_rt/src/regex`), so the comparison is
engine-for-engine; Go #3 is the fastest with Go's `regexp`.

Correctness: every implementation matches the official output at 1000 and the Rust output at
5,000,000 (Velt: both backends, both variants).

## Where the time goes

Timings are in [../RESULTS.md](../RESULTS.md). `main.velt` takes about 1.1× the CPU of Rust #6
on one thread.

- **Matching.** Matching is one JS-style loop (`crates/velt_rt/src/regex/matches.rs`) that runs
  `find_at` without capture tracking when the pattern has no groups or the replacement has no
  `$`, and reuses one `CaptureLocations` otherwise; that is what Rust #6's `find_iter` does.
- **Input.** `readAll` validates the input as UTF-8 and copies it once into the string.
- **Multi-threaded.** The run is dominated by the serial substitution chain, so the
  multi-threaded versions gain little. `main_mt.velt` gives each of the 10 tasks
  `sequence.clone()`: for a string that is another reference (strings count atomically), not a
  copy, so its memory is the substitution chain's intermediate strings.
- Bun (JavaScriptCore's JIT-compiled backtracking regex) is about as fast as Rust here.
