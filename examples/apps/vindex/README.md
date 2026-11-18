# vindex

A disk-based approximate nearest-neighbour index for embedding vectors, written entirely in
Velt. It is a proof of concept for an engine that holds far more vectors than fit in memory:
the index is a few files, a search touches only the parts of them it needs, and the process
itself keeps almost nothing. It was also written to find what Velt lacks for this kind of
program; see [What Velt was missing](#what-velt-was-missing).

```sh
velt build --release                       # target/velt/vindex
alias vindex=$PWD/target/velt/vindex
vindex gen /data/v --n 10000000 --dim 128  # a synthetic data set and queries
vindex truth /data/v                       # exact neighbours of the queries (brute force)
vindex build /data/v --nlist 4096          # the index
vindex bench /data/v --nprobe 1,4,16,64    # recall, latency, throughput
vindex bench /data/v --io pread            # the same without mapping the files
velt test                                  # kernels, exactness, mapped = positioned reads
```

## How it works

An inverted-file index (IVF) with int8 codes and an exact rerank:

```
query ─▶ router: 64 group centres ─▶ centroids of the nearest groups ─▶ nprobe lists
      ─▶ scan the lists' int8 codes (SIMD) ─▶ best `rerank` candidates
      ─▶ exact dot product against each candidate's f32 row ─▶ top k
```

| File of an index | What |
|---|---|
| `vectors.f32` | the rows as unit-length `f32`, in insertion order: the build's input, and what a search reranks against |
| `lists.bin` | the inverted lists back to back; a record is the row id (`u32`) and the row's `dim` int8 codes |
| `centroids.f32`, `meta.json` | the list centroids, the int8 scale and where each list starts |

| Source | What |
|---|---|
| `src/kernels.velt` | the dot products (`f32` and int8, on arrays and on raw bytes), quantization |
| `src/kmeans.velt` | spherical k-means, the assignment step on every worker |
| `src/router.velt` | two-level centroid search: about `sqrt(nlist)` groups of centroids, so a row or a query scores 64 + 8 × 64 centroids instead of 4,096 |
| `src/build.velt` | the build, in three streaming passes: train on a sample, assign every row (each worker reads its own range of the file), write (each worker writes its rows into its own part of every list) |
| `src/index.velt` | search over the mapped files (`MappedFile`), or with positioned reads |
| `src/gen.velt` | the synthetic data set and the brute-force ground truth |
| `src/topk.velt`, `src/store.velt`, `src/rng.velt` | bounded heap, file formats and positioned I/O, a seeded generator |

Nothing holds the vectors in memory. The build keeps the centroids, 4 bytes per row for the
assignment and small per-list write buffers. A search keeps the centroids; everything else is
the system's page cache, which it may drop.

## Results

One machine: 4 virtual cores (an x86-64 server CPU with AVX-512), 15 GB of memory, a virtual
disk, Linux 6.18. The data set is 10,000,000 unit vectors of 128 dimensions (4.9 GB) around
100,000 cluster centres, and 1,000 queries from the same clusters; recall is against the exact
neighbours. The machine is a shared VM and timings moved by up to 30% between runs: compare
ratios, not absolute numbers. This is one data set that fits in the page cache; nothing here
was measured at hundreds of gigabytes (see [Not done](#not-done)).

**Build**

| Step | Time | Peak memory of the process |
|---|---|---|
| Generate 10M rows (4.9 GB) | 4 s | 117 MiB |
| Exact neighbours of 1,000 queries (brute force over the file) | 88 s | 14 MiB |
| Build: train 73 s, assign 58 s, write 5 s | 136 s | 185 MiB |

`lists.bin` is 1.26 GB (132 bytes a row, against 512 for the `f32` row).

**Search**, files in the page cache, k = 10, rerank = 100, 4,096 lists:

| nprobe | recall@10 | records scanned | mapped: ms/query, 1 thread | mapped: queries/s, 4 threads | pread: ms/query | pread: queries/s |
|---|---|---|---|---|---|---|
| 1 | 0.565 | 2,579 | 0.18 | 19,040 | 0.27 | 12,887 |
| 4 | 0.760 | 10,184 | 0.55 | 6,518 | 0.63 | 5,741 |
| 16 | 0.912 | 40,397 | 1.94 | 1,960 | 1.94 | 1,872 |
| 64 | 0.993 | 159,049 | 7.56 | 499 | 7.36 | 493 |
| 128 | 0.999 | 314,854 | 14.5 | 263 | 14.3 | 275 |

- Mapping the files is 1.5× faster where a query is short (the 100 rerank reads are 100
  system calls with `pread`, and none when mapped). On long scans the two are equal: both are
  limited by how fast memory is read, about 47 ns a record against 8–10 ns for the same
  kernel on data that is in the CPU's cache (`bench/kernels.velt`).
- Over a whole benchmark run, `pread` spent 93 s in the kernel and 59 s in the program; the
  mapped run spent 0.8 s in the kernel.
- So the next step for speed is fewer bytes per record (product quantization or 1-bit codes
  ahead of the int8 codes), not a faster int8 kernel.

**Search, files not in the page cache** (dropped before each run; nprobe = 16, one thread,
the average over the 1,000 queries):

| How the files are read | ms/query |
|---|---|
| mapped, and the lists and rows announced with `advise("willneed")` (`--prefetch`) | 3.4–3.7 |
| `pread` | 7.0 |
| mapped, pages read one fault at a time | 19–20 |

The first `pread` run took 56 ms a query: the host had not cached the files either, and only
that run read them from the disk itself. The others show the order, not the speed of a
real disk.

**Kernels**, one query against 8,192 rows in cache, ns per 128-dimension row
(`bench/kernels.velt`, `bench/kernels.c`; three runs each):

| Kernel | Velt, default (`x86-64-v2`) | Velt, `--cpu native` | clang `-O3 -march=native -ffast-math`, plain loop |
|---|---|---|---|
| `f32` dot | 24–25 | 18–24 | 20–26 |
| int8 dot | 9.3–9.9 | 7.6–10.6 | 5.9–7.9 |

Velt's 128-bit kernels are within the noise of what a C compiler makes of a plain loop on
this machine. A hand-written AVX-512 kernel was not measured.

## What Velt was missing

**Added on this branch**

| What | Why |
|---|---|
| `MappedFile` in `velt/fs` ([#1866](https://github.com/velt-lang/velt/issues/1866) area 11; what it leaves out: [#3162](https://github.com/velt-lang/velt/issues/3162)): `MappedFile.open(path)`, `.bytes: shared<u8[]>`, `.f32s(byteOffset, count): shared<f32[]>`, `.advise(...)` | There was no way to map a file. The mapping is an ordinary bounds-checked array: indexing, the SIMD loads and `spawn` all work on it with no copy. Read-only, and never unmapped (see below) |
| the std-only intrinsic `addressArray<T>(address, length): T[]` | An array over memory it does not own (`cap == 0`, the form constant tables already have): what `MappedFile` is built on. Design note in `docs/internals/design/intrinsics.md` |

**Found, not fixed**

| # | Finding | Cost here | Suggestion | Tracked |
|---|---|---|---|---|
| 1 | Passing a `shared` *field* (`dot(r.centres, …)`) takes and drops a counted reference at every call: an atomic add and subtract on a word all threads share | The build's assignment pass took 5.6 s instead of 0.48 s on 4 threads (12×). `bench/shared_field.velt`: 7× on one thread, 37× on four | Borrow the field for the call when its owner is borrowed for the function, as a local is. Until then every hot loop must copy `shared` fields into locals first | [#3113](https://github.com/velt-lang/velt/issues/3113) (point 2) |
| 2 | `xs.sort((a, b) => a - b)` on an `i64[]` is a type error ("expected i64, found f64"), also with `(a - b) as number` | The most common TypeScript comparator does not compile; `(a < b ? -1 : a > b ? 1 : 0)` does | Accept an integer result from a comparator | [#3157](https://github.com/velt-lang/velt/issues/3157) |
| 3 | Bytes cannot be read as another number type without a copy: `readSync` fills only a `u8[]`, and there is no view of a `u8[]` as `f32[]`, `i8[]` or `u32[]` | Every kernel exists twice (`dotF32` and `dotF32Bytes`), built on `u8x16.load(…).asF32x4()`; a `u32` id is assembled from four bytes by hand | `readSync`/`writeSync` on any number array; typed views of a `u8[]`; `MappedFile` views for every number type (`f32s` is the only one) | [#3158](https://github.com/velt-lang/velt/issues/3158) |
| 4 | A mapping cannot be unmapped, written, or grown | An index that is rebuilt or compacted in a long-running server leaks address space; the build writes with `pwrite` | Needs views whose lifetime the compiler tracks (the byte views of `sqlite-engine.md`) | [#3162](https://github.com/velt-lang/velt/issues/3162) |
| 5 | No way to read many file ranges at once without blocking a thread each (`readSync` is synchronous; the async file API is whole-file or stream) | A cold rerank is 100 reads one after another; `advise("willneed")` is the only way to overlap them | Async positioned reads (io_uring, or the blocking pool) | [#3159](https://github.com/velt-lang/velt/issues/3159) |
| 6 | `velt/simd` has no byte shuffle (`pshufb`/`tbl`) and no per-lane population count; vectors are 128-bit only | Not measured: the two operations that 4-bit product quantization ("fast scan") and 1-bit codes are built on, which is where the next speed-up is (the scan is limited by bytes read) | `u8x16.swizzle(table, indices)`, `countOnes()` on integer vectors; 256/512-bit types | [#2416](https://github.com/velt-lang/velt/issues/2416) (shuffle), [#1055](https://github.com/velt-lang/velt/issues/1055) (per-lane `popcnt`), [#1981](https://github.com/velt-lang/velt/issues/1981) (wider vectors) |
| 7 | No parallel loop: every data-parallel pass is "split into ranges, `spawn` each, `Promise.all`, merge" by hand | Written six times in 1,300 lines | `parallelFor(n, (start, end) => …)` or a `map` over ranges in `velt/task` | [#1866](https://github.com/velt-lang/velt/issues/1866) area 6 |
| 8 | No bounded top-k in `velt/collections` (`PriorityQueue` has no replace-top) | `src/topk.velt`, the second program to write it | A `TopK`, or `replaceTop` on `PriorityQueue` | [#3160](https://github.com/velt-lang/velt/issues/3160) |
| 9 | Small: no `process.pid`; Node's `fs.rmSync` is `removeSync`; `f32` has no `toFixed`; `expect(...).toThrow(RangeError)` needs a construct signature type | Each cost a compile error | — | [#3161](https://github.com/velt-lang/velt/issues/3161); `toFixed`: [#3143](https://github.com/velt-lang/velt/pull/3143) |

## Not done

What an engine for hundreds of gigabytes still needs, in the order it would hurt:

- **Training at scale.** k-means here scores every sample row against every centroid. A
  billion rows wants on the order of 100,000 lists or more, which needs hierarchical or
  mini-batch k-means (the router already searches centroids in two levels; training does not).
- **Smaller codes.** 132 bytes a row is 26% of the `f32` data. Product quantization or 1-bit
  codes would bring the scanned bytes down 4–16× (finding 6).
- **Row ids** are 32-bit (4.29 billion rows).
- **Updates.** The index is built once. Inserts, deletes and compaction need a log, a
  mutable tail and a way to replace mapped files (finding 4).
- **A real cold-disk measurement**, on a data set several times larger than memory, on a
  disk whose cache can be controlled.
- macOS is untested here (the code has no Linux-only calls); Windows has no `MappedFile` yet.
