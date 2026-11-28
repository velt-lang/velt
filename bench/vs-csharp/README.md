# Velt against C# on "How fast is C#?"

The nine data-structure programs of [How fast is C#?](https://devmobasa.github.io/how-fast-is-csharp/)
(source: [devmobasa/how-fast-is-csharp](https://github.com/devmobasa/how-fast-is-csharp)), which
take the benchmark of [ben-chmarks.com/gemini-neon](https://ben-chmarks.com/gemini-neon/) and
time an optimized C# ("C# new") against the original C# and ten other languages. Each program is a
data structure plus a driver that runs 10 test cases and prints one JSON line per case (`ops`,
`ns` of the timed region, `checksum`, `verify`).

Each folder here holds a Velt port: `<structure>.velt` (the data structure) and `driver.velt` (a
case-by-case port of the benchmark's C# `Driver.cs`, sharing `common.velt`). The ports print the
same `ops`, `checksum` and `verify` as the C# programs for every case and size.

| folder | structure | entries (n) | algorithm and layout of the Velt port |
|---|---|---:|---|
| `btree` | B-tree | 4,000,000 | C# original: t = 16, 31 keys a node, node pools as flat arrays |
| `dlist` | doubly linked list | 3,000,000 | C# original: node pool with u32 links, sentinel 0, free list |
| `hashmap` | hash map | 2,000,000 | C# original: Fibonacci hashing, linear probing, backward-shift delete |
| `heap` | binary heap | 3,000,000 | C# original: 1-indexed interleaved key/value array, sentinel entry |
| `radix` | radix tree | 1,500,000 | C# original: adaptive nodes (4/8/16/48/256), chunked node pools |
| `rbtree` | red-black tree | 1,500,000 | C# original: no parent links, recorded path, color in the left link |
| `rtree` | R-tree | 400,000 | C# original (its scalar paths): M = 16, quadratic split, 4-ary queue |
| `skiplist` | skip list | 1,250,000 | C# original: u32 node offsets in an 8-byte-unit arena, height pools |
| `unionfind` | union-find | 4,000,000 | C# original: union by size, full path compression |

The ports are plain Velt: arrays and classes where the C# uses unsafe pointers, `stackalloc`,
`Span` or SIMD vectors, scalar loops instead of vector code.

## Running

```sh
python3 bench/vs-csharp/run.py setup WORK      # needs the .NET 11 SDK on PATH
VELT=target/release/velt python3 bench/vs-csharp/run.py run WORK --runs 5
VELT=target/release/velt python3 bench/vs-csharp/run.py cachegrind WORK --scale 20
```

`setup` downloads the C# sources (the original solutions and drivers from ben-chmarks.com, the
optimized solutions from the page's repository) and builds them as the page does: NativeAOT,
`-c Release -p:OptimizationPreference=Speed -p:IlcInstructionSet=native`. `run` builds the Velt
ports with `velt build --release --cpu native` (the counterpart of `IlcInstructionSet=native`),
runs the three programs interleaved, pinned to one CPU, with one warm-up and one timed
repetition of every case per process (the page's `--repetitions 1`), checks that all print the
same results, and prints each case's median, the score (geometric mean of the case medians, as
the page scores) and the peak memory. `cachegrind` counts the Velt programs' instructions at a
smaller n. Results are not kept in the repository.
