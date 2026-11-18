# binary-trees: port notes

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
| `main.velt` | port of Node #7 | `class TreeNode` with `TreeNode \| null` children, per-node allocation |
| `main_mt.velt` | port of Node #6 / Go #2 | one `spawn`ed task per depth + `Promise.all`, printed in order |
| `main_arena.velt` | the shape of Rust #5 | each tree's nodes are Copy structs in an `Arena` pool (`velt/mem`), linked by `u32` index and reset between trees |

Every published Rust binary-trees uses an arena crate (bumpalo, typed_arena or toolshed), which
the rules accept as "a library memory pool"; the idiomatic Velt port allocates per node. Hence the
extra, unpublished `binary-trees_box` (`bumpalo` and `mimalloc` are in `rust/Cargo.toml`).

`expected-quick.txt` is the official `binarytrees-output.txt` (N = 10). Every implementation is
checked byte for byte at N = 10 and, against the Rust output, at N = 21.

## Where the time goes

Timings are in [../RESULTS.md](../RESULTS.md).

- **Per-node allocation.** `main` allocates and frees 2^(d+1) nodes one by one through the
  runtime's mimalloc (`velt_rt_alloc` / `velt_rt_free` call `mi_malloc` / `mi_free` directly).
  `bottomUpTree` compiles to the same instructions as Rust's `bottom_up_tree` apart from the
  allocator call, and `main` uses less CPU than Rust's `Box` + mimalloc version
  (`binary-trees_box`). Against Rust #5, whose bumpalo arena turns each allocation into a pointer
  bump and frees a whole tree at once, `main` takes 3.4× the CPU.
- **Arena.** `main_arena` does what Rust #5 does and uses 0.94× the CPU of the single-threaded
  Rust.
- **Multi-threaded.** `main_mt.velt` has one task per depth (9 equal tasks, like Node #6 and
  Go #2), while Rust #5 also splits each depth's iterations with rayon. Splitting each depth into
  64 spawned chunks did not make it faster, so task granularity is not the limit.
- **Code layout.** The recursive drop glue (`drop TreeNode` → `objdrop TreeNode` →
  `drop Option<TreeNode>`) must stay in one codegen unit; split across units it cost 13–15%
  ([../../RESULTS.md](../../RESULTS.md) "Lessons from optimization work").
