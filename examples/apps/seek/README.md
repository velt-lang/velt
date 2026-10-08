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
```

Output is `path:line:text` (or grouped with `--heading`), with matches highlighted. Velt can't
tell yet whether stdout is a terminal, so colour is on unless `NO_COLOR` is set or you pass
`--color never` (do that when piping). Exit code: 0 found, 1 nothing found, 2 usage error.

Like ripgrep it respects `.gitignore` files (nested ones too, with `!` negation), skips hidden
files and `.git`, and skips binary files (NUL bytes or not UTF-8).
`--hidden`, `--no-ignore`, `--max-depth`, `--max-filesize` and `-j` change that.

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

## Performance (Apple M-series, 10 cores, macOS, warm cache, hyperfine)

| Corpus | seek | ripgrep 14.1 | git grep |
|---|---|---|---|
| velt repo, 3.9k files / 14 MiB, literal | 54 ms | 53 ms | 76 ms |
| velt repo, regex | 54 ms | 62 ms | |
| 36k files / 521 MiB (a pnpm monorepo), literal | 662 ms | 828 ms (`-L`) | |
| same, regex `-i` | 792 ms | 615 ms (`-L`) | |
| same, list files | 221 ms | 107 ms (`-L`) | |

The output is identical to ripgrep's (`rg -n --no-heading`) on every pattern tried, except for
files with NUL bytes: ripgrep prints a match found before the NUL, seek skips the file.

## Known gaps (tracked in #688)

- No `lstat`, so symlinks are always followed (ripgrep doesn't by default; compare with `rg -L`).
  A symlink loop stops at `--max-depth` (default 64).
- `readDir` returns names only, so every entry costs a `stat`; that is most of the gap in
  listing files.
- No `isatty`, so colour can't turn itself off when piped.
