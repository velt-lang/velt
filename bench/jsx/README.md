# TSX fortunes benchmark

The TechEmpower fortunes page (13 rows, `bench/web`'s data) rendered 100 000 times, each
variant in its own process (issue #77):

| variant | what |
|---|---|
| `hand` | template literals, a row per literal, joined once: bench/web's Velt server |
| `precompiled` | the page in TSX ([_page.vlt](_page.vlt)), std/jsx's precompiled templates, `renderToStringSync` |
| `generic` | the same TSX through the generic lowering ([_page_generic.vlt](_page_generic.vlt)) |
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
listed at the top of `run.sh`. Each build first checks that the precompiled and generic pages are the
same, and the same as `hand` apart from the apostrophe (std/jsx writes `&#x27;` as react-dom
does, `escapeHtml` `&#39;`).

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

macOS arm64 (Apple M-series), release (LLVM), 7 runs, 2026-10-08. `BASE=origin/main`: the same
compiler and runtime with `origin/main`'s std (`563bbc34`) against #676's.

| variant | origin/main std: ms | instructions | #676: ms | instructions | Δ instructions |
|---|---:|---:|---:|---:|---:|
| hand | 122 | 2 468 329 889 | 122 | 2 479 257 461 | +0.4% |
| precompiled | 181 | 4 019 035 488 | 157 | 3 338 301 224 | −16.9% |
| generic | 454 | 9 924 249 194 | 465 | 9 919 553 847 | −0.05% |
| rows-strings | 105 | 2 174 219 745 | 109 | 2 196 787 865 | +1.0% |
| rows | 154 | 3 406 964 402 | 123 | 2 594 315 143 | −23.9% |
| rows-render | 171 | 3 734 117 053 | 149 | 3 139 664 603 | −15.9% |

With #77's list fold (`jsxList`: rows without slots are built as strings) and numbers written as
`${n}` instead of through `jsxEscape`, the same compiler and std (macOS arm64, 7 runs):

"vs #676" compares with #676's own row in the table above (same machine, same day, the same
`run.sh`); "vs hand" with this run's `hand` row.

| variant | ms | instructions | vs #676 | vs hand |
|---|---:|---:|---:|---:|
| hand | 122 | 2 476 116 056 | | |
| precompiled | 131 | 2 674 196 739 | −19.9% | +8.0% |
| generic | 458 | 9 873 708 012 | −0.5% | |
| rows | 115 | 2 364 231 467 | −8.9% | |
| rows-render | 143 | 2 949 490 870 | −6.1% | |

What is left of the 8% is copying: the folded rows are joined, the page string is built around
them, and the benchmark adds the doctype with another template literal, where `hand` joins
once. (`rows` and `rows-render` are lists outside a template, so they are not folded.)

With template literals sized once from their parts (#707: one allocation per literal, so a long
part no longer regrows the builder), instructions of one run; the "before" column is the same
benchmark on the main it builds on (#682):

| variant | macOS arm64 before | after | Linux arm64 before | after |
|---|---:|---:|---:|---:|
| hand | 2 490 658 473 | 2 210 258 980 | 2 452 196 306 | 2 176 934 660 |
| precompiled | 2 693 114 024 | 2 343 839 271 | 2 650 557 805 | 2 311 142 969 |
| generic | 9 975 231 533 | 9 246 179 229 | 10 024 439 864 | 9 273 024 533 |

The precompiled page goes from 8% to 6% more instructions than `hand` on both. Linux arm64 is
the bench-arm workflow (`gh workflow run bench-arm -f suite=jsx -f ref=template-builder -f
base=main`, run 37830325119; cachegrind on a GitHub `ubuntu-24.04-arm` runner).

With a template literal reusing the buffer of a long fresh part (a call's result of 1 KiB or
more that nothing else holds: the rows that `jsxList` joined, the page the doctype is put in
front of) instead of copying it into a new one. Linux arm64, cachegrind, `run.sh` with every
variant, the base's compiler and std building the same programs (bench-arm run 37910227799 of
25da008, `VARIANTS="hand precompiled generic shape-no-doctype shape-jsx-escape
precompiled-no-doctype shape" COUNT=valgrind`, base #707 at fd041ac):

| variant | #707 | this | Δ |
|---|---:|---:|---:|
| hand | 2 176 944 855 | 2 185 834 796 | +0.4% |
| precompiled | 2 311 353 151 | 2 267 626 821 | −1.9% |
| generic | 9 285 734 715 | 9 273 225 843 | −0.1% |
| shape-no-doctype | 2 183 555 198 | 2 176 419 265 | −0.3% |
| shape-jsx-escape | 2 241 704 756 | 2 231 462 491 | −0.5% |
| precompiled-no-doctype | 2 250 853 736 | 2 224 027 719 | −1.2% |
| shape | 2 245 227 617 | 2 222 323 309 | −1.0% |

The precompiled page is 3.7% over `hand`. Reusing a buffer saves its allocation and free, but
the text still moves: putting the doctype in front of a 1.2 KB page moves the page with
`memmove`, which costs about as much as copying it. The `+0.4%` on `hand` is the check on every
template literal with a call's result in it (`escapeHtml(f.message)` in each row).

With a `string` child escaped by `jsxEscapeString` (std/jsx's optional export: no `Text` union),
read where it is rather than copied out of its object (`{f.message}`). Linux arm64, the same
command as above (bench-arm run 37910235102 of 5cbd117, base #707 at fd041ac):

| variant | #707 | this | Δ |
|---|---:|---:|---:|
| hand | 2 176 944 844 | 2 185 734 797 | +0.4% |
| precompiled | 2 311 353 164 | 2 220 826 815 | −3.9% |
| generic | 9 285 734 728 | 9 273 125 852 | −0.1% |
| shape-no-doctype | 2 183 555 185 | 2 176 319 283 | −0.3% |
| shape-jsx-escape | 2 241 704 743 | 2 234 562 509 | −0.3% |
| precompiled-no-doctype | 2 250 853 734 | 2 177 227 731 | −3.3% |
| shape | 2 245 227 595 | 2 222 223 315 | −1.0% |

The precompiled page is 1.6% over `hand` (it was 6.2%). Without the doctype it costs what the
same page written by hand in the same shape costs (`precompiled-no-doctype` against
`shape-no-doctype`, +0.04%), and with it slightly less than that page with the doctype
(`shape`). What is left against `hand` is the shape: the page is built around the rows and then
the doctype is put in front of it, which moves the page twice, where `hand` joins one array
once (cachegrind per function: `memmove` +35 M and `prepend_in_place` +22 M against `hand`).
