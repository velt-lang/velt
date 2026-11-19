# TSX fortunes benchmark

The TechEmpower fortunes page (13 rows, `bench/web`'s data) rendered 100 000 times, each
variant in its own process (issue #77):

| variant | what |
|---|---|
| `hand` | template literals, a row per literal, joined once: bench/web's Velt server |
| `precompiled` | the page in TSX ([_page.velt](_page.velt)), velt/jsx's precompiled templates, `renderToStringSync` |
| `generic` | the same TSX through the generic lowering ([_page_generic.velt](_page_generic.velt)) |
| `rows-strings` | only the 13 rows, as strings (`hand`'s rows) |
| `rows` | only the 13 rows, as TSX elements |
| `rows-render` | the rows in a fragment, rendered to a string |
| `shape-no-doctype` | hand-written in the compiled page's shape: the rows joined, then one template literal for the page |
| `shape-jsx-escape` | that, with the messages escaped by `jsxEscape` |
| `precompiled-no-doctype` | the TSX page rendered, without the doctype |
| `shape` | `shape-no-doctype` with the doctype prepended by another template literal, as for the TSX page |

The last four split the difference between `hand` and `precompiled` into steps.

```sh
bench/jsx/run.sh [runs]                  # this checkout
BASE=origin/main bench/jsx/run.sh        # also against origin/main's std/, same compiler and runtime
BASE=origin/main BASE_FULL=1 bench/jsx/run.sh   # also with origin/main's compiler, runtime and std
```

`run.sh` prints the best wall-clock time over the runs and the instructions retired by one run
(macOS `time -l`; Linux `perf stat` when installed, or valgrind's cachegrind with
`COUNT=valgrind`). Instructions are the number to compare on a loaded machine. The other knobs
(`VELT`, `VARIANTS`, `LABEL`, `OUT`, `CG_OUT` for cachegrind files to diff per function) are
listed at the top of `run.sh`. Each build first checks that the precompiled and generic pages
are the same, and the same as `hand` apart from the apostrophe (velt/jsx writes `&#x27;` as
react-dom does, `escapeHtml` `&#39;`).

## Linux arm64: run the bench-arm workflow

No arm64 Linux machine needed: the `bench-arm` workflow (`.github/workflows/bench-arm.yml`)
runs `run.sh` on GitHub's `ubuntu-24.04-arm` runners, release velt (LLVM, clang 18),
`COUNT=valgrind`, for `hand`, `precompiled` and `generic`:

```sh
gh workflow run bench-arm -f suite=jsx -f ref=<branch|tag|sha>              # one ref
gh workflow run bench-arm -f suite=jsx -f ref=<branch> -f base=main          # A/B
gh run watch; gh run view --web                                              # the summary
```

With `base`, the base ref's compiler, runtime and std build the ref's programs. The job summary
has the table (and the precompiled / hand ratio); the raw tables are the `bench-jsx-arm64`
artifact. The cachegrind counts are the acceptance measure; wall time on a shared runner is
indicative only.

## Results

Linux arm64, `run.sh` with `COUNT=valgrind` on a GitHub `ubuntu-24.04-arm` runner (the workflow
now runs only `hand`, `precompiled` and `generic`), instructions of one run; 2026-10-09, commit
c2f05127e.

| variant | instructions | vs `hand` |
|---|---:|---:|
| hand | 2 185 734 797 | |
| precompiled | 2 220 826 815 | +1.6% |
| generic | 9 273 125 852 | +324% |
| shape-no-doctype | 2 176 319 283 | −0.4% |
| shape-jsx-escape | 2 234 562 509 | +2.2% |
| precompiled-no-doctype | 2 177 227 731 | −0.4% |
| shape | 2 222 223 315 | +1.7% |

- Without the doctype the precompiled page costs what the same page written by hand in the same
  shape costs (`precompiled-no-doctype` against `shape-no-doctype`). What is left against
  `hand` is the shape: the page is built around the rows and then the doctype is put in front of
  it, which moves the page twice (`memmove`), where `hand` joins one array once.
- What made the precompiled page cheap: rows without slots are built as strings (`jsxList`),
  numbers are written as `${n}`, a template literal is sized once from its parts and reuses the
  buffer of a long fresh part (a call's result of 1 KiB or more that nothing else holds), and a
  `string` child is escaped by `jsxEscapeString` where it is, without copying it out of its
  object.
- The generic lowering builds element objects and renders them, about 4× the instructions of
  the precompiled page.
