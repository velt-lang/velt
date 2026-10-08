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

```sh
bench/jsx/run.sh [runs]                  # this checkout
BASE=origin/main bench/jsx/run.sh        # also against origin/main's std/, same compiler and runtime
```

`run.sh` prints the best wall-clock time over the runs and the instructions retired by one run
(macOS `time -l`; Linux `perf stat` when installed). Instructions are the number to compare
on a loaded machine. Each build first checks that the precompiled and generic pages are the
same, and the same as `hand` apart from the apostrophe (std/jsx writes `&#x27;` as react-dom
does, `escapeHtml` `&#39;`).

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

The precompiled page still runs 35% more instructions than `hand`. Most of the difference is one
`Element` per row: `rows` costs about 300 instructions more per row than `rows-strings`.
