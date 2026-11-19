# Open performance findings

Gaps to Rust found with the benchmarks that the backends and `velt_opt` cannot close alone,
because they come from lowering (`crates/velt_vir/src/lower`), the standard library (`std/`) or a
missing language guarantee. Each item gives the evidence and the proposed change. Only open
findings are listed; a fixed one is removed. Measurements are in [RESULTS.md](RESULTS.md) and
[benchmarks-game/RESULTS.md](benchmarks-game/RESULTS.md); VIR comes from
`velt build --release --emit vir bench/<name>.velt`.

## 1. Integer `sort()`: the pivot is reloaded on every element (std)

Affects `sort()` on number arrays. Sorting 1M numbers retires 427M instructions against 264M for
Rust's `sort_unstable` (1.62×, RESULTS.md "Sorting with a comparator"); the whole `sort`
benchmark, setup included, is at parity.

pdqsort (`std/lang/sort/pdq.velt`) partitions around the pivot at `xs[lo]` and moves elements only
with `arraySwap`. The branchless Lomuto loop reads `less(xs[i], xs[lo])`, and because every
`arraySwap` may write `xs[lo]` as far as the optimizer can tell, the pivot is loaded again on each
iteration; the element in flight cannot stay in a register either. Proposed: for element types
that need no drop (every number type), read the pivot into a local before the loop and swap it
into place after, as Rust does; for other types, move the pivot out of the array and back with
the same owned-once discipline `arrayMove` gives the stable sort (`std/lang/sort/stable.velt`).

## 2. Benchmarks Game programs whose idiomatic loop shape is slower (no compiler fix)

The idiomatic ports use the simpler loop shapes of the Node and Go programs. A Rust program with
exactly the Velt loops runs at parity or slower, so the gap is the algorithm, not code
generation. CPU time against the single-threaded Rust program
([benchmarks-game/RESULTS.md](benchmarks-game/RESULTS.md)):

| program | Velt `main` × | same-shape Rust | Rust-shaped Velt × |
|---|---:|---|---|
| mandelbrot | 5.24 | parity | `main_opt` (Copy struct of 8 `f64` lanes, vectorized) 1.55 |
| spectral-norm | 1.75 | parity | `main_opt` (two-lane Copy struct) 1.18 |
| fannkuch-redux | 1.44 | parity | `main_mt` on Rust #4's algorithm: 1.06 CPU, 1.10 wall |
| n-body | 1.71 | Rust with `Box<Body>` is slower | `main_opt` 1.85 |

n-body is also about 1.3× a Rust `Vec` version that compiles to the same optimized IR. On x86_64,
clang 22 and clang 18 compile `main.velt` and `main_opt.velt` to equally fast code (1.5–1.6×
Rust #3 without `target-cpu`), so the LLVM version does not explain the gap; it has not been
measured on Apple silicon. n-body `main_opt` keeps its arrays in memory because they are built by
`push` in a loop: the buffer pointer is a phi of `alloc` / `realloc` results, which LLVM's alias
analysis does not see through. Building them preallocated (`new Array<T>(n).fill(v)`) removes the
extra loads and stores.

## 3. Open gaps tracked in issues

- `%` on a `number` returned from a function is a `noinline` call, so a constant divisor becomes
  a 64-bit `idiv`; `closures_borrowed` is slower than Node because of it
  ([#1470](https://github.com/velt-lang/velt/issues/1470)).
- Callbacks over an aliased `string[]` share and release the element on every call, about 55
  instructions each; pinning the buffer once per method call would remove that
  ([#592](https://github.com/velt-lang/velt/issues/592)).
- JSON `navigate` and `parse_value` are slower than Node, and `bench/typical`'s `record` (1.41×)
  and `consume` (3.5×) slower than Rust (survey
  [#529](https://github.com/velt-lang/velt/issues/529)).
