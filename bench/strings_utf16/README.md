# Non-ASCII strings: Velt vs Node

Phase 0 of the JavaScript string semantics change (#377, design:
[docs/internals/design/strings.md](../../docs/internals/design/strings.md)): workloads over
non-ASCII text, with a Node baseline, recorded before the representation changes so later phases
can be measured against them.

Each part is a Velt program (`<part>.vlt`) and the same program for Node (`<part>.js`). Text is
generated with the same LCG in both, so runs are deterministic; each prints one checksum line
(lengths, counts, a hash, a sample).

| part | measures |
|---|---|
| `char_code_at` | `for (let i = 0; i < s.length; i++) s.charCodeAt(i)`, twice over each of three 2M-character texts: Latin with ~5% accented letters, CJK, and emoji-heavy (one character in three an emoji, including `❤️` and `👍🏽`) |
| `sort` | `sort()` without a comparator on 200k words of 2–8 characters from ASCII, accented Latin, CJK, Hangul, emoji, `ﬁ` and `€` |
| `csv` | 100k lines with non-ASCII fields: `join("\n")`, `split("\n")`, `split(",")` per line, re-joined with `";"` and `"\n"` |
| `json` | `JSON.stringify` and `JSON.parse<Item[]>` (Node: `JSON.parse`) of 50k objects whose strings hold accents, CJK and emoji, 3 rounds |
| `append` | 20 strings of about 190k characters, each built by 100k `s += piece` appends of non-ASCII pieces, with a strided `charCodeAt` checksum (the in-place append of #381) |

## Running

```sh
bench/strings_utf16/run.sh [runs] [only-part]   # default 5 runs
```

It builds a release `velt`, builds each part with `--release --backend cranelift` and
`--release --backend llvm` (`n/a` when a build fails), and prints the best wall-clock time per
part. Needs cargo, node, python3, and clang for the LLVM column.

**Outputs match Node** since #377 phase 2b: the programs are plain TypeScript with JavaScript's
meaning, and Velt strings now count and index UTF-16 code units and sort by them. The last
column reports it and must say `yes` for every part.

## Results

`bench/strings_utf16/run.sh 3` on a shared 4-core cloud container (Intel Xeon @ 2.80GHz, Linux
x86_64), Node v22.22. Best of 3, milliseconds including process start; the machine is noisy, so
compare ratios.

After #377 phase 2b (UTF-16 semantics; the outputs match Node):

| part | Velt cranelift release | Velt LLVM release | Node | output matches Node |
|---|---:|---:|---:|---|
| append | 89 | 78 | 158 | yes |
| char_code_at | 291 | 256 | 320 | yes |
| csv | 117 | 64 | 287 | yes |
| json | 139 | 105 | 282 | yes |
| sort | 104 | 81 | 349 | yes |

The same runs of `main` just before phase 2b (2586621, byte semantics, outputs differ):

| part | Velt cranelift release | Velt LLVM release | Node | output matches Node |
|---|---:|---:|---:|---|
| append | 67 | 53 | 160 | no |
| char_code_at | 154 | 135 | 330 | no |
| csv | 67 | 62 | 296 | no |
| json | 111 | 111 | 292 | no |
| sort | 90 | 66 | 348 | no |

- `char_code_at` now indexes code units: non-ASCII text calls the runtime, which steps from the
  thread's last position in the same string (`str/recent.rs`); with breadcrumbs alone the part
  takes about 3.2 s. Strength reduction (phase 3) removes the call.
- `sort` compares by code units; `append` and `csv` pay for code-unit `charCodeAt`, `slice`
  and `split` translations on non-ASCII text.
