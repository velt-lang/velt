# binary-trees — port notes

> The timing tables below were measured while other agents shared the machine. The authoritative
> numbers are the serial run in `../RESULTS.md`.

## Sources and credits
| file | source | notes |
|---|---|---|
| `../rust/src/bin/binary-trees.rs` | [Rust #5](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/binarytrees-rust-5.html) — the Rust Project Developers, TeXitoi, Cristi Cobzarenco, Matt Brubeck, Tom Kaitchuck, Volodymyr M. Lisivka, Ryohei Machida | unchanged (bumpalo arenas, rayon) |
| `../rust/src/bin/binary-trees_st.rs` | same | the single-thread reference: #5 unchanged except `main()` first pins rayon's global pool to one thread |
| `../rust/src/bin/binary-trees_box.rs` | not published; derived from #5 | `Box` nodes on mimalloc, plain loops, 1 thread: the same-allocator comparison for the idiomatic Velt port (which allocates/frees every node through the runtime's mimalloc) |
| `main.go` | [Go #2](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/binarytrees-go-2.html) — Gerardo Lima, Diogo Simoes et al. | goroutine per depth; gofmt only |
| `main_st.go` | [Go #1](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/binarytrees-go-1.html) — the Go Authors, Kevin Carson, Isaac Gouy | fastest single-threaded Go; gofmt only |
| `main.js` | [Node #6](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/binarytrees-node-6.html) — Léo Sarrazin, Andrey Filatkin | `worker_threads`, one worker per depth; unchanged |
| `main_st.js` | [Node #7](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/binarytrees-node-7.html) — Léo Sarrazin, Andrey Filatkin, Isaac Gouy | sequential version of #6; unchanged |
| `main.vlt` | port of Node #7 | `class TreeNode` with `TreeNode \| null` children, per-node allocation |
| `main_mt.vlt` | port of Node #6 / Go #2 | one `spawn`ed task per depth + `Promise.all`, printed in order |

Every published Rust binary-trees uses an arena crate (bumpalo, typed_arena or toolshed), which
the rules accept as "a library memory pool"; the idiomatic Velt port allocates per node. Hence the
extra, unpublished `binary-trees_box` (`bumpalo` and `mimalloc` are in `rust/Cargo.toml`).

`expected-quick.txt` is the official `binarytrees-output.txt` (N = 10). Checked byte-for-byte:
every implementation at N = 10 and at N = 21 against the Rust output (Velt LLVM and Cranelift,
`main` and `main_mt`; the three Rust binaries agree with each other at N = 21).

## Timings (Apple M4 4P+6E, N = 21, best of 5)
Measured while other agents were building and benchmarking (load average 12–55 on 10 cores):
walls are inflated and noisy. CPU = user + sys. These numbers were taken before the Rust binaries
were settled: the Rust rows come from builds of the same code in a scratch project (`_st` was
measured as a sequential build without rayon rather than a 1-thread pool); "ref" rows aren't in the
tree. A serial `run.sh` pass replaces them.

Single-threaded:
| implementation | wall s | CPU s | peak RSS MB |
|---|---:|---:|---:|
| Rust `binary-trees_box` (Box + mimalloc) | 6.60 | 6.42 | 159.9 |
| ref Rust Box + mimalloc through `velt_rt_alloc`-shaped wrappers | 6.90 | 6.74 | 159.9 |
| Rust `binary-trees_st` (#5, bumpalo, 1 thread) | 2.90 | 2.79 | 165.7 |
| **Velt LLVM `main`** | 7.54 | 7.43 | 159.5 |
| Velt Cranelift `main` | 18.94 | 18.02 | 159.5 |
| Go `main_st` (#1) | 13.38 | 25.08 | 200.8 |
| Node `main_st` (#7) | 16.04 | 18.18 | 1071.4 |
| Bun `main_st` (#7) | 7.91 | 8.25 | 927.2 |

(The two Box + mimalloc rows and Velt LLVM were run back to back; an earlier Velt LLVM batch in
heavier load had best 11.38 s wall / 9.39 s CPU.)

Multi-threaded:
| implementation | wall s | CPU s | peak RSS MB |
|---|---:|---:|---:|
| ref Rust Box + mimalloc, rayon | 1.55 | 7.09 | 165.3 |
| Rust `binary-trees` (#5 unchanged: bumpalo, rayon) | 0.80 | 3.38 | 278.5 |
| **Velt LLVM `main_mt`** | 2.29 | 9.89 | 158.0 |
| Velt Cranelift `main_mt` | 4.30 | 18.48 | 158.2 |
| Go `main` (#2) | 5.72 | 27.33 | 511.3 |
| Node `main` (#6) | 3.60 | 12.10 | 1227.3 |
| Bun `main` (#6) | 3.36 | 12.06 | 1260.3 |

## Gaps vs Rust and root causes
(Rust `Box` on macOS's system `malloc`, dropped from the tree, was 2–2.8× slower than Velt —
17.19 s CPU single-threaded, 4.69 s wall with rayon — so the allocator dominates this benchmark.)

1. **Velt vs `binary-trees_box` (Box + mimalloc), single thread: 1.16× CPU (below the 1.2× bar).** Codegen is
   the same shape; `bottomUpTree` compiles to the same instructions as Rust's `bottom_up_tree`
   apart from the allocator call:
   ```
   bl _velt_rt_alloc            ; Rust: bl _mi_malloc_aligned (inlined __rust_alloc)
   ```
   `velt_rt_alloc` is an out-of-line `extern "C"` call into the runtime staticlib that re-validates
   the `Layout`, calls `__rust_no_alloc_shim_is_unstable_v2`, then `mi_malloc_aligned`;
   `velt_rt_free` does the same checks before tail-calling `mi_free`. Routing Rust through
   identically shaped wrappers costs +5% (6.42 → 6.74 s); the remaining ~10% is not pinned down
   under this noise. Fix (runtime/codegen): for constant size/align (every `new C()`), call
   `mi_malloc(size)` / `mi_free(p)` directly (align ≤ 16 is guaranteed by mimalloc), skipping the
   wrapper and the aligned path, or ship the runtime as LLVM bitcode so the wrapper inlines.
   **Update:** done (FINDINGS 8.3): the wrappers call `mi_malloc` / `mi_free` directly for
   alignments up to 8 (mimalloc guarantees 8, not 16, for every block size). 8.31 → 6.75 CPU s
   on the Windows machine (bench/RESULTS.md "Backend round"); not yet re-measured against
   `binary-trees_box` here.
2. **Velt vs Rust #5 (`binary-trees_st` / `binary-trees`, bumpalo): 2.7× single-thread CPU, 2.9× multi-thread wall.**
   Root cause: bumpalo turns each allocation into a pointer bump and frees a whole tree at once;
   Velt allocates and frees 2^(d+1) nodes one by one (the idiomatic port must allocate per node;
   the rules also accept "a library memory pool"). Fix (std): a `std/arena` region type (a library
   pool, allowed by the rules) for an optimized variant; long term, escape analysis could give a
   tree that never leaves a function one region instead of per-node frees.
3. **Multi-threaded Velt vs Rust `Box` + mimalloc + rayon (ref, not in the tree): 1.48× wall, 1.4× CPU.** Part is (1).
   The structure differs too: `main_mt.vlt` has one task per depth (9 equal tasks, like Node #6 /
   Go #2) while Rust #5 also splits each depth's iterations with rayon. Splitting each depth into 64
   `spawn`ed chunks (scratch experiment) didn't help (2.50 s wall, 10.29 s CPU), so it isn't task
   granularity. The extra CPU over the single-thread ratio is not root-caused: measured under heavy
   contention from other agents, so it needs a quiet rerun.

Cranelift is 2.4× slower than LLVM (not investigated; LLVM is the `--release` default).

## Friction a TypeScript developer hits
- **Nullable fields can't be narrowed**: `if (node.left === null) return 1; itemCheck(node.left)`
  fails with `expected TreeNode, found TreeNode | null`, and the documented workaround (narrow a
  local copy) fails too: `const left = node.left;` → `cannot move a field out of a class instance`.
  The port uses `this.left?.check() ?? 0`. Pending golden:
  `tests/golden/bugs/sema_narrow_nullable_field.vlt`.
- **Constructor parameter properties** (`constructor(public left: TreeNode | null, …) {}`, planned
  in docs/internals/design/ts-alignment.md §5) don't parse yet (`expected ':'`), so fields are declared and
  assigned by hand.
- `Math.max(6, n)` is `f64`-only, so an `i64` max is a ternary; `parseInt` returns `f64`
  (`parseInt(argv[0]) as i64`).
- `main_mt.vlt` was straightforward: `spawn(work(...))` + `await Promise.all(tasks)` reads like
  the TS version, with no worker-thread boilerplate or message passing.

## Std and runtime round
`main_arena.vlt` is the shape of Rust #5 (bumpalo): each tree's nodes are Copy structs in a
`std/arena` pool, linked by `u32` index and reset between trees. It runs at 0.94× the CPU of
the single-threaded Rust, against 3.43× for the per-node-allocation `main`.
