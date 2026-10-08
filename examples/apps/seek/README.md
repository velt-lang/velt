# seek

A fast recursive text search (grep / ripgrep style), written entirely in Velt.

```sh
velt build --release             # target/velt/seek
alias seek=$PWD/target/velt/seek
seek useState                    # search the current directory
seek -i 'use[a-z]+\(' src        # regex, case-insensitive, under src/
seek -w -t ts -t tsx Button      # whole word, only .ts/.tsx files
seek -l TODO                     # only the file names
seek -f '\.test\.'               # list files (like fd), filtered by a regex
seek -s unsafe > /dev/null       # stats: files, MiB, time, threads
velt test                        # unit tests: matcher, .gitignore rules, options
velt run demo.vlt                # the whole pipeline on a generated tree (golden: demo.out)
./bench.sh                       # seek vs ripgrep vs git grep on this repository (hyperfine)
```

Output is `path:line:text` (or grouped with `--heading`), with matches highlighted. Velt can't
tell yet whether stdout is a terminal, so colour is on unless `NO_COLOR` is set or you pass
`--color never` (do that when piping). Exit code: 0 found, 1 nothing found, 2 usage error.

It respects `.gitignore` files (nested ones too, with `!` negation), skips hidden files and
`.git`, and skips binary files. `--hidden`, `--no-ignore`, `--max-depth`, `--max-filesize` and
`-j` change that.

**Where it differs from ripgrep:**
- **Which ignore files it reads.** It reads `.gitignore` files even outside a git repository.
  It doesn't read `.gitignore` files in parent directories, `.git/info/exclude`, the global
  gitignore, `.ignore` or `.rgignore`.
- **Which files it skips.** Files over 50 MiB are skipped by default (`--max-filesize`). A file
  that isn't valid UTF-8 or that contains a NUL byte is skipped entirely; ripgrep prints the
  matches it finds before the NUL.
- **Symlinks** are always followed (there's no `lstat` yet, #691). ripgrep needs `-L` for that.
- **`-w`** wraps the pattern in `\b…\b`. For a pattern that starts or ends with a non-word
  character, that matches differently from ripgrep's `-w`.
- **Exit code** is 0 (found) or 1 (nothing found) even after an I/O error; ripgrep exits 2.

## How it works

```
roots ─▶ walk tasks (N) ─▶ channel<FileJob> ─▶ search tasks (N) ─▶ channel<Report> ─▶ printer
          ▲      │ subdirectories
          └──────┘ channel<Pending>
```

| File | What |
|---|---|
| `src/walk.vlt` | N tasks share one queue of directories; a `shared` counter of pending directories tells the last one to close the queue. `.gitignore` rules travel with each directory as plain data, and each task compiles and caches its own regexes |
| `src/search.vlt` | N `spawn`ed workers read files (sync reads, on their own thread), search, and format output in parallel |
| `src/matcher.vlt` | a plain word uses `indexOf` (memchr/memmem in the runtime), anything else a `RegExp` (linear time). It finds the first match in the whole file before splitting out lines, so a file without a match costs one scan |
| `src/ignore.vlt` | gitignore globs → regexes, nested files, `!` negation, directory-only rules |
| `src/app.vlt` | the pipeline as `execute(argv, noColor, write, writeErr)`; the printer writes in 64 KiB batches |
| `src/options.vlt` | the `velt:cli` parser |
| `src/main.vlt` | process glue: args, `NO_COLOR`, stdout/stderr, exit code |
| `tests/*.test.vlt` | `velt test` |
| `demo.vlt` / `demo.out` | the pipeline on a generated tree; a golden in `cargo test -p veltc --test golden` |

## Performance

`./bench.sh` builds seek, checks that it and ripgrep print the same lines, then times both and
`git grep` on this repository with hyperfine (20 runs, warm cache). Apple M-series (4
performance + 6 efficiency cores), macOS, ripgrep 14.1.1 built with `cargo install`:

| Search (3.9k files, 14 MiB) | seek | ripgrep | git grep |
|---|---|---|---|
| literal `unsafe` | 41.5 ± 0.4 ms | 41.0 ± 0.8 ms | 45.4 ± 0.3 ms |
| regex `fn \w+_(index\|slice)` | 41.8 ± 0.3 ms | 43.9 ± 9.3 ms | |
| `-i` regex | 49.3 ± 14.0 ms | 46.4 ± 19.6 ms | |
| list files (`--files`) | 13.8 ± 0.6 ms | 11.6 ± 0.9 ms | |

So content search on a tree this size is level with ripgrep, and listing files is about 20%
slower. On a larger tree (36k files, 521 MiB, not in the repository, so indicative only),
`-i` regex was about 29% slower than ripgrep (#699), and listing files 2× slower: there every
entry needs a `stat` (#692).

## Known gaps (from #688)

- No `lstat` (#691), so symlinks are always followed, and a symlink loop stops only at
  `--max-depth` (default 64).
- `readDir` returns names only (#692), so every entry costs a `stat`. That is most of the gap
  in listing files.
- No `isatty` (#693), so colour can't turn itself off when piped.
