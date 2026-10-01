# regex-redux

> The timing tables below were measured while other agents shared the machine. The authoritative
> numbers are the serial run in `../RESULTS.md`.

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
| `main.vlt` | idiomatic Velt: `readAll()` into a string, `new RegExp(p, "g")`, `.matches(s).length`, `.replace(s, r)` | | 1 |
| `main_mt.vlt` | `main.vlt` with the substitution chain and each variant count in spawned tasks (like Rust #6's `rayon::join` + `par_iter`) | | 10 tasks |

Why not the fastest published programs: Rust #7 (0.78 s) binds PCRE2 (`pcre2_sys`, `libc`) and
Go #5 / #4 bind PCRE through third-party cgo modules; the bench project only has rayon and
regex, and Go builds single files with the standard library. Rust #6 is the fastest Rust
program that uses the `regex` crate (the engine under Velt's `std/regex`, so the comparison is
engine-for-engine), Go #3 the fastest with Go's `regexp`.

Correctness: every implementation matches the official output at 1000 and the Rust output at
5,000,000 (Velt: both backends, both variants).

## Timings (Apple M4, 10 cores; best of 2 sessions × 5 runs; stdout to a file)

The machine was shared with other agents during both sessions (load average 12–35), so wall
times are inflated and noisy; CPU seconds are the more reliable column. Versions: velt 0.1.0 (8f35554), rustc 1.98.1 (`-C target-cpu=native`, LTO), go 1.27.1, node 24.11.1, bun 1.4.2.

| Implementation | wall s | CPU s | RSS MB |
|---|---:|---:|---:|
| Rust regex-redux (#6, rayon) | 0.724 | 0.819 | 201.2 |
| Rust regex-redux_st | 0.837 | 0.811 | 200.9 |
| Velt LLVM main | 1.017 | 0.989 | 224.9 |
| Velt Cranelift main | 1.158 | 1.120 | 225.0 |
| Velt LLVM main_mt | 1.156 | 1.266 | 473.2 |
| Velt Cranelift main_mt | 0.872 | 1.019 | 514.0 |
| Go main (#3, goroutines) | 13.765 | 41.324 | 396.8 |
| Node main (#3, 1 worker) | 1.957 | 2.518 | 1047.6 |
| Bun main (same source) | 0.597 | 0.731 | 438.0 |

Bun (JavaScriptCore's JIT-compiled backtracking regex) is the fastest here. The run is
dominated by the serial substitution chain (~1 s), so the multi-threaded versions barely gain.

## Gap: Velt main vs Rust #6 single-threaded, ~1.2×

Phase times of `main.vlt` (`performance.now()`, ms) vs the same phases in Rust with the
`regex` crate, using `find_iter` (what Rust #6 does) or `captures_iter`:

| phase | Velt | Rust `find_iter` | Rust `captures_iter` |
|---|---:|---:|---:|
| read stdin | 44 | — | — |
| clean `>.*\n\|\n` | 110 | 91 | 114 |
| `tHa[Nt]` | 16 | 13 | 14 |
| `aND\|caN\|Ha[DS]\|WaS` | 12 | 9 | 9 |
| `a[NSt]\|BY` (3.5M matches) | 335 | 204 | 281 |
| `<[^>]*>` | 454 | 286 | 434 |
| `\|[^\|][^\|]*\|` | 274 | 170 | 261 |

Velt tracks the `captures_iter` column. **Root cause (runtime):** `velt_rt_regex_replace`
(`crates/velt_rt/src/regex/replace.rs`) iterates `re.captures_iter(s)` for every replacement so
it can expand JS `$` patterns, and `velt_rt_regex_exec_all` (behind `matches` / `matchAll`) does
the same. Resolving capture groups for each of millions of matches costs ~40–50% on the
match-heavy substitutions. **Fix:** when the replacement contains no `$` (checked once per
call), iterate `re.find_iter(s)` and append the replacement bytes directly; likewise use
`find_iter` in `exec_all` when the pattern has no capture groups (`captures_len() == 1`). By the
table above that should remove most of the ~0.3 s gap to Rust #6 (not measured: it needs a
runtime rebuild).

Smaller: `readAll` (`crates/velt_rt/src/stdin.rs`) does `String::from_utf8_lossy(&buf)
.into_owned()`, which copies the 50 MB input a second time even when it is valid UTF-8;
`String::from_utf8(buf)` with a lossy fallback avoids the copy.

## Multi-threaded version
`main_mt.vlt` gives each of the 10 tasks its own `sequence.clone()` (async parameters are owned,
and `shared<string>` can't be passed where a `string` is read: `expected string, found
shared<string>`), so it peaks at 473–521 MB vs Rust's 201 MB. Proposal (as in
k-nucleotide/NOTES.md): let `shared<T>` be read like a borrowed `T`.

## Friction for a TypeScript developer
- Module-level `const VARIANTS = [...]` is rejected ("module-level constants must be constant
  expressions"): the pattern tables are locals of `main`.
- No regex literals (`/agggtaaa|tttaccct/g` is a parse error; filed as
  `tests/golden/bugs/parse_regex_literal.vlt`); `new RegExp("…", "g")` works the same.
- `seq.match(re)` with `g` is spelled `re.matches(seq)`; `str.replace(re, s)` is
  `re.replace(str, s)` (receiver swapped vs JS).
- `let replaced = sequence;` moves `sequence`, so its length has to be read into a local first
  (JS would keep both names).
