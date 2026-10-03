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

**Outputs differ from Node until #377 phase 2 lands.** The programs are plain TypeScript with
JavaScript's meaning; today Velt counts and indexes UTF-8 bytes and sorts bytewise, so every
length, hash and sort order involving non-ASCII text differs. The last column reports this
(`yes`/`no`) and is not a failure; once strings are UTF-16 code units, every row must say `yes`.
With every non-ASCII character replaced by an ASCII letter, Velt and Node print the same output
today.

## Results

`bench/strings_utf16/run.sh 3` on a shared 4-core cloud container (Intel Xeon @ 2.80GHz), Node
v22.22, with the runtime of `main` at b80571c (before #377 phase 1). Best of 3, milliseconds including process start; the machine is noisy,
so compare ratios.

| part | Velt cranelift release | Velt LLVM release | Node | output matches Node |
|---|---:|---:|---:|---|
| append | 8269 | 8285 | 160 | no |
| char_code_at | 155 | 137 | 329 | no |
| csv | 67 | 107 | 284 | no |
| json | 107 | 106 | 290 | no |
| sort | 85 | 68 | 344 | no |

- `append` is quadratic in Velt: every `s += piece` copies the string (#381); Node appends to a
  rope.
- The other parts do byte work in Velt today; with UTF-16 semantics, `char_code_at` and `sort`
  pay for code-unit indexing and comparison, which is what these numbers are the baseline for.
