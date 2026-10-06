# Benchmark results

`pwsh bench/run.ps1 -Runs 10` (Linux/macOS: `bench/run.sh 10`). Best wall-clock time of 10
runs in milliseconds, including process start (about 10 ms on this machine for an empty Velt or
Rust program). Every configuration prints the same output as the Rust version; the harness checks this.

| benchmark | Velt cranelift debug | Velt cranelift release (+velt_opt) | Velt LLVM release | Rust -O | Node |
|---|---|---|---|---|---|
| classes | 317 | 244 | 201 | 293 | 521 |
| closures | 425 | 282 | 231 | 287 | 854 |
| fib | 74 | 58 | 32 | 32 | 147 |
| floats | 73 | 74 | 61 | 55 | 124 |
| hashmap | 263 | 207 | 182 | 287 | 479 |
| loops | 1007 | 471 | 182 | 199 | 1363 |
| nbody | 1216 | 350 | 162 | 179 | 1826 |
| sort | 193 | 162 | 113 | 92 | 950 |
| strings | 271 | 254 | 246 | 248 | 468 |

M1 kernels:
- **fib**: recursive `fib(35)`.
- **loops**: the longest Collatz chain for starts below 1M, plus a 4000×4000 multiply/modulo loop.
- **floats**: a 600×400 Mandelbrot (200 iterations max) plus 20M-step midpoint integration of 4/(1+x²).
- **int32**: the 32-bit hash loop of issue #521 on numbers, 2×50M steps as written (`(y * k) | 0`,
  which JS rounds through a double) and 2×50M with `Math.imul(y, k)`. Rust computes the same
  values (`bench/rust/int32.rs`: the rounding by an `i64`→`f64`→`i64` round trip, then
  `wrapping_mul`). See "JS int32 operators" below.

M2 programs (the Rust and Node versions are the same algorithm, written idiomatically):
- **nbody**: the Benchmarks Game n-body, 5M steps; Copy structs `Vec3` / `Body` in an array,
  small struct methods (`add`, `sub`, `scale`, `dot`), `Math.sqrt`.
- **hashmap**: 1M inserts and 1M lookups (half misses) in a `Map<i64, i64>`, then a word count
  of 1M generated words in a `Map<string, i64>` (`upsert` per word). Rust uses std's
  `HashMap` (SipHash) and `entry()`; bench/rust/hashmap_fx.rs is the same with an FxHash
  hasher (see "Prelude work (Map)").
- **sort**: `sort()` of 1M pseudo-random i64 and of 200k generated strings. Rust uses
  `sort_unstable` (Velt's `sort()` is unstable too).
- **classes**: 20 passes of virtual `area()` calls over a `Shape[]` of 1M mixed subclasses
  (plus a non-overridden method), then 20 passes of interface calls over a `Scorer[]` of 1M
  structs. Rust uses `Box<dyn Trait>` for both.
- **closures**: 20 rounds of `map` / `filter` / `reduce` / `forEach` over 1M elements with
  capturing arrows, then 20M calls of a stored (escaping) closure. Rust materializes every
  `map` / `filter` into a `Vec` like JS does, and stores the closure as `Box<dyn Fn>`.
- **strings**: 1M template-literal lines, `join("\n")`, then a `charCodeAt` scan of the 24 MB
  result; then 100k `s += "…"` appends and 100k `` t = `${t}…${i}…` `` appends. Rust uses
  `format!`, `join`, a byte loop, `push_str` and `write!`. (The table's row predates the
  appends, which add a few milliseconds to the Velt columns now that they append in place.)
- **shapes**: 1M shapes in a discriminated union (`{ kind: "circle"; r } | ...`), 40 passes of
  `switch (s.kind)` plus a `s.kind === "empty"` test. Rust uses an enum and `match`, Node plain
  objects and `switch`.

Setup: Intel i9-12900HK laptop, Windows 11, LLVM/clang 22.1.8, rustc 1.93.1 (`rustc -O`, the
x86-64 baseline target like Velt), Node 22.22.0. The machine is noisy: on repeated runs the
numbers vary by about ±15% (more while other builds run), so differences below that are noise.

How to read it:
- The LLVM release build is within noise of Rust -O everywhere except `sort` (below), and
  faster on `classes` and `hashmap`, where the Rust versions pay for a `Box` per object and for
  SipHash.
- `sort` is 1.2× Rust (see "Prelude work" below; the `sort` and `strings` rows are from a later
  run than the rest of the table, on a busier machine; the `hashmap` row is from the Map work
  below).
- The Cranelift release build is 1.1–2.6× slower than LLVM release: Cranelift does not
  vectorize, hoist loop invariants or turn constant divisions into multiplies.
- The Cranelift debug build (velt_opt off, Cranelift `opt_level=none`) spends its extra time
  where release inlining matters: `loops` (hardware divides, nothing hoisted), `nbody` (every
  `Vec3` method is a call on stack memory) and `closures`.

## Apple silicon (stream A, 2026-09-30)

`bench/run.sh 10` on an Apple M4 (4 performance + 6 efficiency cores, 32 GB), macOS 26.6, Apple
clang 21 (LLVM backend), rustc 1.98.1 (`rustc -O`), Node 24.11.1. Same programs, same output check.

| benchmark | Velt cranelift debug | Velt cranelift release (+velt_opt) | Velt LLVM release | Rust -O | Node |
|---|---|---|---|---|---|
| classes | 152 | 134 | 112 | 146 | 251 |
| closures | 260 | 253 | 190 | 177 | 496 |
| fib | 33 | 33 | 18 | 17 | 70 |
| floats | 47 | 47 | 36 | 36 | 60 |
| hashmap | 125 | 108 | 85 | 113 | 287 |
| loops | 419 | 266 | 148 | 147 | 1250 |
| nbody | 1282 | 215 | 107 | 113 | 1035 |
| sort | 108 | 93 | 49 | 39 | 383 |
| strings | 151 | 135 | 116 | 95 | 222 |

- The picture matches x86_64: LLVM release is at Rust -O parity (ahead on `classes` and
  `hashmap` for the same reasons), `sort` is 1.26× Rust. `strings` is 1.22× here, a wider
  gap than on the Windows machine.
- Cranelift release is 1.0–2.0× LLVM release on aarch64, a narrower spread than on x86_64
  (`loops` 1.8× vs 2.6×), probably because aarch64's hardware divide is cheaper, so LLVM's
  division-to-multiply rewrite saves less (not profiled).
- Cranelift debug `nbody` is 6× its release build, from the stack traffic of unoptimized
  `Vec3` method calls, as on x86_64.

## M2 optimization work

Before/after for the M2 benchmarks (best of 7 before, best of 10 after, same machine):

| benchmark | cranelift release before → after | LLVM release before → after | Rust -O |
|---|---|---|---|
| nbody | 1132 → 350 | 278 → 162 | 179 |
| sort | 235 → 189 | 206 → 170 | 81 |
| closures | 380 → 282 | 276 → 231 | 287 |
| classes | 231 → 244 | 216 → 201 | 293 |
| hashmap | 273 → 255 | 222 → 223 | 284 |
| strings | 332 → 332 | 275 → 279 | 239 |

What changed (all in `velt_opt` and the two backends):
- **Runtime math as instructions** (`velt_codegen_llvm/src/runtime.rs`,
  `velt_codegen_cl`): `Math.sqrt` / `floor` / `ceil` / `trunc` / `abs` were opaque calls to
  `velt_rt_math_*`, which blocked vectorization and forced every value through memory around
  the call. They are now `llvm.sqrt.f64` etc. (LLVM) and `sqrt` / `floor` / … instructions
  (Cranelift). Other pure runtime functions (`Math.round`, `**`) are declared
  `memory(none)`, the read-only ones (`velt_rt_str_cmp`, `velt_rt_str_eq`,
  `velt_rt_hash_bytes`) `memory(read)`, and `velt_rt_alloc` / `velt_rt_realloc` return
  `noalias` memory. nbody's step loop now keeps all 5 bodies in vector registers.
- **Closure specialization** (`velt_opt/src/const_fields`): the code pointer of a closure
  `{ code, env }` is propagated into the loads `(*f).0` of its readers, also through
  read-only pointer params, and callees receiving a known closure are cloned (memoized, so
  recursive helpers such as `introsort` → `partition` form one specialized family). constfold
  then turns the indirect calls into direct ones and the inliner inlines them: no indirect
  calls remain in `sort` or `closures` (Rust gets the same from monomorphizing over closure
  types).
- **Pointer forwarding** (`velt_opt/src/addr_forward.rs`): a pointer local that always holds
  `&a.f…` is replaced by the place itself, which un-escapes struct temporaries passed as
  `this` / by-pointer arguments after inlining.
- **Scalar replacement of aggregates** (`velt_opt/src/sroa`): non-address-taken aggregate
  locals (nested ones level by level) are split into per-field locals, so struct math lives
  in registers under Cranelift too (nbody 3.2× faster there). Enum aggregates, whose payload
  is only reachable through variant views, are left in memory.
- **Cold calls stay calls** (`velt_opt/src/inline`): functions that never return (the
  out-of-bounds panic helper) are no longer inlined into every bounds check.

Gaps that need lowering, prelude or contract changes (vtable dispatch, stack envs for
non-escaping closures, `noalias` for `mut` borrows, the sort algorithm, string `==`) are in
[FINDINGS.md](FINDINGS.md).

## Prelude work (sort, join)

`sort()` is now a pattern-defeating quicksort (std/prelude/sort.vlt) and `join` builds its
result with the runtime string builder (std/prelude/array.vlt). Time of the operation alone
(`performance.now()` around it, best of 7 in one process, LLVM release; Rust in the same
harness):

| operation | before | after | Rust |
|---|---|---|---|
| `sort()` 1M random i64 | 88 ms | 32 ms | 15 ms (`sort_unstable`) |
| `sort()` 200k strings | 43 ms | 30 ms | 23 ms (`sort_unstable`) |
| `sort()` 1M random i64, Cranelift release | 107 ms | 68–82 ms | |
| `join("
")` of 1M lines (24 MB) | 63 ms | 10 ms | |

Whole `sort` benchmark (wall clock, best of 10): LLVM 170 → 100–113 ms against Rust's
85–92 ms, so 1.2× instead of 2×. What mattered:
- **Branchless Lomuto partitioning** (as in Rust's current `sort_unstable`): the comparison
  result is added to the boundary index instead of branching on it. Comparing *before* the
  swap is essential: comparing the element after swapping it into place makes every
  iteration wait on the previous iteration's store (82 → 32 ms from that change alone).
  Strings gain too (43 → 30 ms): their comparison is a call, but the mispredicted branches of
  Hoare partitioning cost more.
- `bool as usize` instead of `c ? 1 : 0`: Cranelift keeps the ternary as a branch (164 → 82
  ms under Cranelift).
- Tried and dropped: BlockQuicksort (offset buffers per 64-element block). With only
  `__intrinsic_array_swap` available and no `noalias` on the array (FINDINGS.md §3), every
  offset store forces the array header to be reloaded: 44 ms vs 32 ms for Lomuto. Also tried
  for `sortBy`: merging index permutations through a scratch buffer and applying them along
  cycles; it was slower than SymMerge for objects (random access per swap), so `sortBy`
  keeps SymMerge and only skips merging runs that are already in order.
- The remaining 2× on integers is codegen: per element the Lomuto loop reloads the array's
  data pointer and length and does three bounds checks (FINDINGS.md §3), and elements can
  only be swapped, not held in a register (no generic move out of an array).

## `noalias` for `mut` params

Sema now enforces exclusive `mut` access, so lowering marks params (vir.rs invariant 9):
`mut` aggregates and out-pointers `noalias`, shared borrows `readonly`, all of them `nonnull` +
`dereferenceable(size)`. The LLVM backend emits these as parameter attributes, and
`velt_opt` promotes the scalar fields behind a `noalias` param (an array's data pointer and
length) into locals, stored back / reloaded only around calls that receive the param (LLVM
cannot do this itself: the data pointer is loaded from memory and the array pointer escapes
into the recursive pdqsort calls). In the Lomuto loop the array header now stays in registers
and one of the two bounds checks is gone.

Before/after on a loaded machine (best of 3×7 runs alternating the two builds; Rust's times
in the same session were 30–60% above the table at the top, so compare within a row):

| benchmark | LLVM release before → after | Cranelift release before → after | Rust -O |
|---|---|---|---|
| `sort()` 1M i64 alone (best of 7 in-process) | 27.3 → 25.0 ms | 63 → 65 ms | |
| `sort()` 200k strings alone | 26.5 → 26.7 ms | 38 → 38 ms | |
| sort | 159 → 149 | 202 → 203 | 153 |
| nbody | 226 → 220 | 407 → 408 | 220 |
| strings | 253 → 243 | 281 → 288 | 283 |
| closures | 282 → 284 | 324 → 316 | 321 |
| hashmap | 217 → 221 | 262 → 243 | 317 |

Integer sorting gains about 9%; everything else is within noise (nbody's and closures' hot
loops were already fully inlined into functions without pointer params). The rest of the gap
to Rust's `sort_unstable` is the pivot reload and remaining bounds check (FINDINGS.md §3).

## Prelude work (Map)

`Map<K, V>` (std/prelude/map.vlt) keeps its insertion-ordered layout (dense entry arrays plus
an open-addressing index) with these changes:
- **Packed index words**: each slot is one `u64`, the top 32 bits of the scrambled hash
  (`hash * 2^64/phi`) plus the entry position. A probe rejects non-matching slots without
  loading the key, and since the home slot is the top bits of the same product, growing,
  shrinking and deleting read homes from the slot words: `entryHashes` and `entryLive` are
  gone and no key is ever rehashed.
- **One probe per `set`**: the lookup returns either the slot of the key or where a new entry
  goes, so an insert no longer probes twice.
- **Robin Hood linear probing**, grown at 3/4 load: clusters stay sorted by home slot, so a
  miss stops at the first resident that is closer to its home than the key would be; deletes
  shift the cluster back (still no index tombstones).
- **`upsert` / `update` / `getOrInsert`** (single lookup read-modify-write), used by the
  benchmark's word count like Rust's `entry().or_insert(0) += 1`.
- `delete` drops the value immediately (values are stored as `V | null`, null = tombstone);
  compaction renumbers the index in place, and shrinks it first when a draining map leaves
  it mostly empty (repeated compactions used to rescan the full-size index).
- One empty-map check per operation instead of two; string keys compare with `==`
  (`velt_rt_str_eq`).

Whole benchmark (wall clock, best of 12, interleaved runs, LLVM release):

| hashmap.vlt | ms |
|---|---|
| Velt, old Map (get + set word count) | 214 |
| Velt, new Map, same source (get + set) | 191 |
| Velt, new Map, `upsert` word count (current bench) | 170 |
| Rust -O, std HashMap (SipHash) | 259 |
| Rust -O, std HashMap + FxHash (bench/rust/hashmap_fx.rs) | 219 |

Per operation (`performance.now()` around each phase, best of 7 in one process and of 8
processes; 1M ops each, the benchmark's 787k-key `Map<i64, i64>`; word count with get + set
in Velt and `entry()` in Rust):

| phase | old Map | new Map | Rust SipHash | Rust FxHash |
|---|---|---|---|---|
| insert (1M `set`, 787k keys) | 54 | 42 | 49 | 36 |
| hit (1M `get`) | 24 | 23 | 42 | 21 |
| miss (1M `get`) | 14 | 12 | 19 | 6 |
| delete (1M `delete`, 787k hits) | 49 | 43 | 52 | 24 |
| word count (1M strings) | 102 | 102 | 156 | 174 |
| miss, 2M, 600k keys (load 0.57) | 57 | 33 | | |

Notes:
- Rust's word count is slower than Velt's because `format!` is slower than a template
  literal; the map part is a small share of it.
- Robin Hood at 3/4 against plain linear probing grown at 5/8 (both with packed words):
  equal on the whole benchmark within noise (187 vs 181 ms in one run), linear probing a bit
  faster on hits because at 787k keys it already sits in a twice larger index, Robin Hood
  faster on misses at equal load (33 vs 36–38 ms above) and half the index memory between
  5/8 and 3/4 load. Robin Hood grown at 7/8 was no faster.
- Tried and dropped: keys and values interleaved in one entry array (`Entry<K, V>` structs)
  to save a cache miss per hit: no measurable gain, and larger elements to swap when
  compacting.
- The remaining gap to hashbrown + FxHash on misses and deletes is memory: hashbrown probes a
  1-byte control array (2 MB here) where Velt probes 8-byte slot words (16 MB), and a
  delete touches the slot, the key and the value (3 cache misses against hashbrown's 2).

## Async

`pwsh bench/async/run.ps1 -Runs 5` (Linux/macOS: `bench/async/run.sh 5`). Every program is in
`bench/async/`: the Velt source, `node/<name>.js`, and a tokio crate in `rust/` (one binary per
benchmark; the argument `current` selects tokio's current-thread runtime instead of the
multi-thread one). Velt is built with `velt build --release` (LLVM) and run on its default
runtime (one worker per core) and with `VELT_THREADS=1`. The harness checks that all five
configurations print the same output. Same machine as above, Node 22.22.0, rustc 1.93.1,
tokio 1.53, futures 0.3 (Rust uses the system allocator; the Velt runtime links mimalloc). The
machine was running other builds, so expect ±15–20%.

| benchmark | Velt (all cores) | Velt (1 thread) | Rust tokio multi-thread | Rust tokio current-thread | Node |
|---|---|---|---|---|---|
| await_chain | 55 | 57 | 52 | 52 | 508 |
| await_deep | 379 | 335 | 433 | 502 | 743 |
| fanout_all | 138 | 129 | 145 | 133 | 114 |
| spawn_many | 614 | 697 | 703 | 453 | 1618 |
| timers | 70 | 102 | 64 | 3210 | 229 |

Best of 5, wall-clock ms including process start. Peak working set (MB, max over the runs):

| benchmark | Velt (all cores) | Velt (1 thread) | Rust tokio multi-thread | Rust tokio current-thread | Node |
|---|---|---|---|---|---|
| await_chain | 6.4 | 5.7 | 4.6 | 4.4 | 57.7 |
| await_deep | 6.4 | 5.7 | 4.7 | 4.5 | 71.7 |
| fanout_all | 7.2 | 6.2 | 4.9 | 4.7 | 71.7 |
| spawn_many | 436.1 | 435.1 | 394.6 | 402.5 | 893.6 |
| timers | 45.9 | 44.9 | 29.1 | 28.6 | 168.0 |

Before the `Promise.all` budget fix ([FINDINGS.md](FINDINGS.md) §7), same session, best of 3
(a configuration whose first run took over 10 s ran once): spawn_many 220514 ms (all cores) /
329755 ms (1 thread), 530 / 457 MB; timers 1247 / 3187 ms, 59 / 48 MB. The other rows were
within noise of the table above.

The programs:
- **await_chain**: 10M sequential `await step(acc, i)` of an async function that returns at once.
- **await_deep**: `await deep(20, i)` 500k times; `deep` awaits itself recursively (21 async
  frames per call, 10.5M in total).
- **fanout_all**: 1000 rounds of `Promise.all` over 1000 calls of a small async function
  (Velt: lazy promises, started by the join; Rust: `futures::future::join_all`, no spawn;
  Node: eager promises).
- **spawn_many**: 1M `spawn(work(i))` (trivial arithmetic plus one `yieldNow()`), then
  `Promise.all` over the handles (Rust: `tokio::spawn` + `yield_now`, handles awaited in order;
  Node has no spawn: a task is a started async call, and `yieldNow` is a `setImmediate`
  promise, a real trip through the event loop).
- **timers**: 100k concurrent `sleep(1)` calls joined with `Promise.all` (Rust: `join_all` of
  `tokio::time::sleep`; Node: `setTimeout` promises).
- **hot_loop** (added with the backend round below): 40 rounds of `await` plus a 5M-iteration
  loop in `async main` that pushes bytes to a `u8[]` and sums them, so the loop's state lives
  in the async frame (the reverse-complement shape).

How to read it:
- **await_chain / await_deep: Velt is at or ahead of Rust.** An awaited `async` call is a
  direct call into a state machine embedded in the caller's state, inlined like a plain call:
  the same loop with a synchronous `step` takes the same time (53 vs 45 ms, best of 7 in one
  session). Node pays a promise and a microtask per `await` (6.6×). Recursion needs one heap
  frame per level in both Velt and Rust; Velt is probably ahead because of mimalloc (not
  profiled).
- **fanout_all**: at parity with Node and ahead of Rust. One core is enough (the children never
  suspend), so the all-cores and 1-thread columns are within noise.
- **spawn_many and timers: at tokio level.** Both were quadratic (220 s and 1.2 s) because
  `velt_rt_all` polled its children with tokio's cooperative budget spent
  ([FINDINGS.md](FINDINGS.md) §7, fixed). Now no child is polled without budget and the join
  pays one unit per finished child. spawn_many matches tokio multi-thread (which awaits the
  handles in order); on one thread it is ~1.5× tokio current-thread. timers matches tokio
  multi-thread on all cores and stays linear on one thread, where Rust's idiomatic `join_all`
  hits the same trap and takes 3.2 s (tokio's `block_on` polls the root on a non-worker thread
  in the multi-thread runtime, where wakes are not deferred, but on the current-thread runtime
  the budget applies). `bench/async/rust/src/bin/repro_all_budget.rs` reproduces the old join
  next to the fixed one.
- **Memory**: an idle Velt process is ~2 MB above Rust (mimalloc arenas, one worker thread per
  core) and ~50 MB below Node. For 1M live tasks Velt needs about 430 B per task against
  tokio's ~390 B and Node's ~840 B (the `FuturesUnordered` node and the `JoinObj` box on top of
  tokio's task cell; the state sits inline in a 64-byte class). Before the fix, up to one
  cloned waker per pending child sat in tokio's defer list, for 450–520 B per task.

Where each implementation allocates (Velt from `velt build --release --emit vir`):

| benchmark | Velt | Rust tokio | Node |
|---|---|---|---|
| await_chain | nothing: `step`'s 32-byte state is a field of `main`'s state | nothing: the child future is inline | a promise per call, a microtask per `await` |
| await_deep | one `velt_rt_fut_box` per level (48-byte header + 32-byte state), polled through `velt_rt_fut_poll` | one `Box::pin` per level | a promise and a suspended async frame per level |
| fanout_all | per child: a boxed state (`velt_rt_fut_box`) and a `FuturesUnordered` node; per round: the promise array (grown by `realloc`), the results buffer, the `velt_rt_all` leaf and the boxed `Promise.all` state | per child: a `FuturesUnordered` node (`join_all` over 30+ futures is a `FuturesOrdered`); the futures live inline in one `Vec`, plus the output `Vec` | per child: an (already resolved) promise; per round: the array, `Promise.all`'s result array and resolve closures |
| spawn_many | per task: tokio's task cell with the state inline (64-byte class), the join handle box (`JoinObj`) and a `FuturesUnordered` node | per task: tokio's task cell; the `JoinHandle` is a pointer in the `Vec` | per task: the async frame and its promise, plus a promise and an immediate per `yieldNow` |
| timers | per child: the boxed `nap` state, a leaf box holding tokio's `Sleep` (`velt_rt_sleep`), a `FuturesUnordered` node | per child: a `FuturesUnordered` node; `Sleep` lives inline in the `nap` future | per child: a promise, a `Timeout` object and the resolve closure |

## Discriminated unions (language agent, 2026-09-30)

Payload enums + `match` were replaced by discriminated unions + `switch`; `match` on tags and
integers now lowers to one VIR `Switch` (jump table) instead of a compare chain. No other
benchmark used enums or `match`. `bench/shapes`, best of 10, same machine, back to back:

| shapes | Cranelift release | LLVM release | Rust -O |
|---|---|---|---|
| before: payload `enum Shape { Circle(f64), ... }` + `match` | 162 | 86 | 74 |
| after: discriminated union + `switch (s.kind)` | 145 | 78 | 74 |

Full `pwsh bench/run.ps1 -Runs 5` before and after the change on a busier machine (±15% noise):
LLVM release classes 191 → 194, closures 252 → 244, fib 35 → 40 (Rust 32 → 40), floats 55 → 66
(Rust 58 → 65), hashmap 159 → 196, loops 193 → 193, nbody 171 → 167, sort 87 → 94, strings
114 → 127; shapes (new) 92 vs Rust 83 — the changes track the Rust column (machine load).

## Semantics stage 1: strings as values + JS division (language agent, 2026-09-30)
Gate run (docs/internals/design/semantics.md "Gates") on a Windows workstation (i9-12900HK, Windows 11),
same session: baseline = the compiler at `cb4f324` (built from a separate export of the tree),
candidate = this branch. `pwsh bench/compare.ps1 -Baseline <base velt.exe> -Runs 15` builds
both with LLVM release and runs them interleaved; median wall-clock ms incl. process start.

| benchmark | baseline | candidate | change |
|---|---|---|---|
| classes | 190.6 | 188.1 | -1.3% |
| closures | 254.0 | 247.4 | -2.6% |
| fib | 34.1 | 33.7 | -1.2% |
| floats | 62.2 | 61.5 | -1.1% |
| hashmap | 173.8 | 158.7 | -8.6% |
| loops | 195.5 | 196.8 | +0.7% |
| nbody | 173.0 | 175.5 | +1.4% |
| shapes | 87.5 | 87.7 | +0.3% |
| sort | 96.0 | 92.6 | -3.5% |
| strings | 130.9 | 110.1 | -15.9% |

`bench/async` (`-Dir bench/async -Runs 11`, same method): await_chain 103.1 → 96.6 (-6.3%),
await_deep 1223 → 1129 (-7.7%), fanout_all 506 → 497 (-1.8%), spawn_many 113125 → 101823
(-10.0%; the known `Promise.all` pathology, see "Async"), timers 1987 → 1999 (+0.6%).

Where `strings` gains: `charCodeAt` is inline code (was a runtime call per byte: the scan phase
went from ~31 to ~11 ms), numbers formatted into strings are inline (no heap), and the builder
appends to a buffer it owns without the `Vec` round trip. `hashmap` gains from `velt_rt_str_hash`
hashing inline keys without a pointer chase.

Refcount operations (`pwsh bench/rc_stats.ps1`: release-built programs linked to the debug
runtime, `VELT_RC_STATS=1`): **0 retains and 0 shared releases in every benchmark**, bench/async
included. `strings` allocates 1,000,001 heap string buffers (the 1M template results of 24–26
bytes, and the joined text) and frees all of them; every other benchmark allocates none (their
strings are static or inline). The baseline runtime has no counters (it deep-copied on every
`.clone()`).

HTTP (`pwsh bench/http/compare.ps1 -Rounds 5`: `examples/http_hello.vlt`, `oha -z 10s -c 256`
on the same machine, A/B alternating): baseline median 53,000 req/s / 18.6 MB peak working set,
candidate 52,289 req/s / 19.2 MB — within the round-to-round spread (baseline 49.1k–54.4k,
17.6–19.3 MB; candidate 50.1k–54.2k, 18.6–19.9 MB).

Soak (`-SoakSeconds 600`, candidate, 256 connections, 10 minutes, 31.9M requests at 53k req/s):
private bytes 29.6 MB at 1 min, 29.4–30.9 MB throughout, 29.9 MB at the end; working set
17.0 → 19.3 MB (peak 19.9 MB). No growth trend in private memory: no leak.

## Semantics stage 2: shared references (2026-10-01)
Gate ([semantics.md "Gates"](../docs/internals/design/semantics.md#gates),
[semantics-stage2.md §8](../docs/internals/design/semantics-stage2.md#8-gates-and-accounting))
on an i9-12900HK laptop, Windows 11, with no builds or tests running (one background desktop
app used about 1.5 of 20 hardware threads). Baseline = `main` at bde7b1d, candidate = this
branch; both LLVM release, interleaved A/B runs, medians of wall-clock ms including process
start. The evidence is in three layers:

1. **Representation.** No benchmark shares a value, so lowering counts no type in any of them
   (`VELT_DEBUG_COUNTED=1` prints an empty set for all 26 programs below): no reference count,
   no box, `noalias`/`readonly` unchanged.
2. **Generated code.** LLVM IR of baseline and candidate (`--release --emit llvm`, panic-location
   strings normalized, since the two checkouts' std paths differ in length) is **byte-identical**
   for 20 of the 26 programs: fib, floats, hashmap, loops, shapes, sort, strings; all six of
   bench/async; binary-trees, fannkuch-redux, fasta, mandelbrot, n-body, pidigits,
   spectral-norm. The others differ only in: classes, k-nucleotide, regex-redux,
   reverse-complement — vtables gain one hidden slot (`SLOT_SHARE`; in the three Benchmarks
   Game programs only the vtables of thrown error objects, used on the error path), so method
   offsets move by 8 bytes; closures — heap closure environments carry a count word (one store
   at creation, a load and branch at release); nbody — `Vec3`/`Body` are no longer Copy
   (structs are objects), so they are passed by reference instead of by copy.
3. **Interleaved timings.**

`pwsh bench/compare.ps1 -Runs 21` (bench/) and `-Runs 15 -Dir bench/async`:

| benchmark | baseline | candidate | change | IR |
|---|---|---|---|---|
| classes | 216.2 | 213.3 | -1.4% | vtable slot |
| closures | 257.8 | 254.5 | -1.3% | env count |
| fib | 52.7 | 52.4 | -0.4% | identical |
| floats | 75.1 | 75.9 | +1.0% | identical |
| hashmap | 211.1 | 203.0 | -3.9% (rerun, 31 runs: -3.2%) | identical |
| loops | 228.6 | 222.2 | -2.8% | identical |
| nbody | 187.6 | 187.7 | +0.0% | by reference |
| shapes | 121.9 | 123.2 | +1.1% | identical |
| sort | 104.1 | 106.7 | +2.5% | identical |
| strings | 129.3 | 128.2 | -0.9% | identical |
| async/await_chain | 67.5 | 69.7 | +3.2% (rerun, 31 runs: -5.7%) | identical |
| async/await_deep | 306.6 | 311.6 | +1.6% | identical |
| async/fanout_all | 159.0 | 157.7 | -0.8% | identical |
| async/hot_loop | 155.9 | 156.5 | +0.3% | identical |
| async/spawn_many | 637.5 | 646.4 | +1.4% | identical |
| async/timers | 102.0 | 98.7 | -3.2% (rerun, 31 runs: +2.5%) | identical |

`pwsh bench/benchmarks-game/compare.ps1 -Runs 9` (official N; reruns `-Only <program> -Runs 9`):

| program | baseline | candidate | change | rerun | IR |
|---|---|---|---|---|---|
| binary-trees | 7259 | 7203 | -0.8% | | identical |
| fannkuch-redux | 33710 | 33186 | -1.6% | | identical |
| fasta | 12695 | 15107 | +19.0% | -19.2% | identical |
| k-nucleotide | 18609 | 17042 | -8.4% | +9.8% | error vtables |
| mandelbrot | 31507 | 29741 | -5.6% | +2.6% | identical |
| n-body | 4097 | 3940 | -3.8% | +3.1% | identical |
| pidigits | 3274 | 2640 | -19.4% | -0.8% | identical |
| regex-redux | 1598 | 1585 | -0.9% | | error vtables |
| reverse-complement | 20305 | 17888 | -11.9% | +16.1% | error vtables |
| spectral-norm | 2026 | 2226 | +9.9% | -0.3% | identical |

Every program whose generated code changed is within ±3% in the micro suite (classes -1.4%,
closures -1.3%, nbody +0.0%). Every result outside ±3% is either on byte-identical IR (hashmap,
await_chain, timers, fasta, mandelbrot, n-body, pidigits, spectral-norm) or on IR that differs
only off the hot path (k-nucleotide, reverse-complement), and its sign flips on a rerun. Even
with no other builds running, this laptop's Benchmarks Game timings drift by up to 2–5× between
runs (thermal and turbo state; fasta, k-nucleotide and reverse-complement also stream 250 MB
through files), so those rows measure the machine, not the compiler. On the IR evidence the
gate holds: no benchmark's code got slower.

Refcount operations: the string counters (`pwsh bench/rc_stats.ps1`) are unchanged (no
benchmark shares an object; counted types are empty). New: the debug runtime's `VELT_RC_STATS=1`
line ends in `blocks=A/F`, every `velt_rt_alloc`/`velt_rt_free` block; the stage 2 goldens marked
`// check: no leaks` assert A = F (no leaks), and every debug golden runs under the checking
allocator. Compile time: programs that share nothing lower once (`lower` of tests/golden/m1/hello
0.6 → 0.8 ms); a program that shares lowers two or three times (the counted-type fixpoint;
tests/golden/lang/share_aliasing: 4.3 ms). Sema of examples/http_hello: 5.5 → 6.3 ms (median of
6 interleaved release builds): the shared-value, cell and assigned-field passes.

## Compile time

`pwsh bench/compile/run.ps1 -Runs 10` (Linux/macOS: `bench/compile/run.sh 10`): the front end of
`velt build -v --emit vir` — load + parse, sema, lowering to VIR — best of 10 runs per stage, in
milliseconds (Windows, release `velt`). `http_hello` is examples/http_hello.vlt (std/http + the
prelude), `all_std` imports every std module, `units_1000` / `chain_1000` are 1000 generated units
(bench/compile/unit.tmpl: a class and an overriding subclass, a struct implementing a shared
interface, functions modifying an array param; call chains of 8, resp. one chain through the whole
program).

Sema regression (2026-09-30): "mutation is inferred" (c4064de) made sema 45× slower. The
ownership fixpoint (`velt_sema::ownership::infer`) reported a change every round for every
function with a param it takes ownership of (it compared the HIR param mode, which is only synced
after the fixpoint, instead of the inferred one), so every build ran its 1000-round safety cap —
1000 passes over every body, prelude included — and programs whose modifications travel more than
1000 calls deep were left under-inferred. Fixed, and the fixpoint is now a worklist: bodies visited
callees first, revisited only when a function they read changes (≈1.5 visits per body), dispatch
groups joined once built. Two quadratic lookups went too: `find_impl` scanned every impl of the
program (now indexed by interface), and vtable layout scanned every overridden method per class.

| program | before: parse | before: sema | after: parse | after: sema |
|---|---|---|---|---|
| http_hello | 12.3 | 294.2 | 8.5 | 4.9 |
| all_std | 14.6 | 332.9 | 13.1 | 5.9 |
| units_1000 | 45.8 | 18502.6 | 48.1 | 154.4 |
| chain_1000 | 41.6 | 16313.9 | 47.3 | 149.5 |

Before = a8285e7, after = this change (http_hello / all_std parse and sema from 20 interleaved
runs of each binary; parse also lost a `canonicalize` per module read). For reference, sema of
http_hello before "mutation is inferred" (5702c53) was 5.3 ms on this machine (2.4 ms on the
Apple silicon machine). Sema now grows linearly — `cargo test -p velt_sema --release --test scaling
-- --ignored --nocapture`: 100 / 1000 / 5000 units in 20 / 162 / 843 ms with call chains of 8,
18 / 166 / 872 ms with one chain through the program. (With only the change-flag fix and the old
round-based fixpoint, one chain of 1000 still took 17.8 s: a round over every body per call
level.)

Lowering to VIR (`velt_vir::lower`) and VIR verification are now the super-linear stages:
1000 / 2000 / 5000 units lower in 217 / 572 / 3047 ms and verify in 48 / 129 / 782 ms.

## Backend round: allocator, async frames, divisions (perf agent, 2026-09-30)

FINDINGS.md 8.3, 8.2 part 2 and 8.8, on a Windows workstation (i9-12900HK, Windows 11, clang
22), while other agents were building: numbers are CPU seconds (user + sys of the process) as
min / median of interleaved runs (A B A B …), which is steadier than wall time under that load.
Before = the compiler and runtime at `01ecac6`; "runtime only" = the old compiler linked against
the new runtime (`VELT_RT_LIB`), which separates the two parts of 8.3. The Benchmarks Game
programs run with their official N. reverse-complement reads the 25M fasta file through a copy
of `main.vlt` that opens the file by path (`/dev/stdin` does not exist on Windows); same loop.

| program | runs | before | runtime only | after | change (min) |
|---|---:|---|---|---|---:|
| binary-trees (N = 21) | 5 | 8.31 / 9.23 | 6.75 / 7.30 | 6.94 / 7.73 | −17% |
| n-body (5e7) | 9 | 2.23 / 2.38 | 2.25 / 2.34 | 2.30 / 2.33 | noise |
| n-body `main_opt` (5e7) | 9 | 2.34 / 2.44 | 2.17 / 2.45 | 2.23 / 2.44 | noise |
| reverse-complement (25M) | 7 | 1.27 / 1.47 | | 1.06 / 1.20 | −16% |
| bench/async/hot_loop | 7 | 0.41 / 0.42 | | 0.13 / 0.14 | −69% |
| bench/async/hot_loop, Cranelift release | 7 | 0.42 / 0.44 | | 0.28 / 0.28 | −33% |
| spectral-norm (5500) | 7 | 1.28 / 1.39 | | 1.22 / 1.36 | noise |
| spectral-norm (3000), Cranelift release | 7 | 0.45 / 0.50 | | 0.41 / 0.48 | −10% |

- **Allocation (8.3).** The runtime part is the whole binary-trees gain: `velt_rt_alloc` /
  `velt_rt_free` now call `mi_malloc` / `mi_free` (alignment ≤ 8) without building a `Layout`
  or taking mimalloc's aligned path. The LLVM allocator attributes (`allockind`, `allocsize`,
  `"alloc-family"`, malloc-like memory effects) change nothing measurable here; n-body
  `main_opt` keeps its 228 loads per step in the optimized IR because its buffers are nested
  phis of `alloc` / `realloc` results, which LLVM's alias analysis does not look through
  (FINDINGS 8.3).
- **Async frames (8.2 part 2).** `velt_opt::frame_slots` keeps the frame fields a poll
  function reads in a loop in locals. `bench/async/hot_loop.vlt` (new, with Rust and Node
  versions) is the reverse-complement shape made measurable: a 5M-iteration byte loop in
  `async main` between awaits, pushing to a frame-resident `u8[]` and summing into a frame
  `i64`. In its optimized IR the loop used to load `total`, `bytes.length` and
  `bytes.capacity` from the frame and store `total` and `bytes.length` back on every iteration;
  now its only memory access is the store of the pushed byte. For reference, the tokio
  version (`bench/async/rust`, current-thread runtime) takes 514–586 ms wall against Velt'
  145–215 ms after (437–528 before) in five alternating runs: rustc's generator keeps the
  loop's state in its frame too, since a generator's `self` is not `noalias`.
- **Divisions (8.8).** `velt_opt::divisions` turns spectral-norm's `(…) / 2` into an `ashr`; on
  x86 the loop is bound by the floating-point chain, so only Cranelift (hardware `idiv`) gains.

The rest of `bench/` and `bench/async` (`pwsh bench/compare.ps1 -Runs 11`, wall-clock medians,
and `-Dir bench/async`) is unchanged within noise: classes 224.8 → 224.9 ms, closures 298.6 →
295.8, fib 46.7 → 45.4, floats 72.8 → 73.8, hashmap 193.8 → 193.1, loops 211.0 → 209.0, nbody
192.8 → 193.1, shapes 103.7 → 103.6, sort 101.4 → 101.0, strings 125.7 → 128.6 (21 runs each for
the last four; the CPU-time rerun of strings, 15 runs, has equal medians); await_chain 68.1 →
66.5, await_deep 432.2 → 429.5, fanout_all 146.3 → 146.0, spawn_many 659.2 → 667.3, timers
77.3 → 78.7. The two passes add about 0.8 ms to `optimize` for examples/http_hello.vlt (7.2 →
8.0 ms median of 11; the whole release build takes seconds, mostly in clang and the linker).

## Compiler perf round 3: class params, poll frames, compile-time scaling (2026-10-01)

Same machine (i9-12900HK, Windows 11, clang 22), shared with other agents' builds and gate runs
all night, so every number is noisy. Before = `3438f72` (the backend round), after = this
round's branch.

### Code quality (FINDINGS 8.1, 8.2 part 1)

CPU seconds, min / median of interleaved runs, LLVM release, official N:

| program | runs | before | after | change (min) | IR |
|---|---:|---|---|---:|---|
| fasta (25M) | 11 | 4.69 / 5.11 | 4.56 / 4.95 | −3% | `rng: Random` now `noalias`; seed kept in a register |
| pidigits `main_limbs` (10000) | 11 | 1.58 / 1.72 | 1.64 / 1.78 | noise | `other` `readonly` |
| bench/async/hot_loop | 15 | 0.125 / 0.156 | 0.141 / 0.172 | noise | frame `noalias` (15.6 ms CPU tick) |
| n-body (5e7), `main_opt` | 7 | 2.39 / 2.56, 2.28 / 2.66 | 2.52 / 2.67, 2.53 / 2.56 | noise | identical |
| binary-trees (21) | 5 | 7.25 / 9.05 | 7.44 / 8.80 | noise | identical |
| spectral-norm (5500) | 7 | 1.25 / 1.41 | 1.22 / 1.39 | noise | identical |

The rows with identical IR measure the noise floor tonight: ±5–11% on the minimum. The `bench/`
micro suite (`pwsh bench/compare.ps1 -Runs 11`) compiles to identical IR for every program and
moved by −10% … +11% (wall-clock medians), i.e. noise.

### Compile time

New: `velt build --timings` (`-v` plus each optimizer pass and the LLVM backend's IR printing
and clang time), and bench/compile/run.ps1 / run.sh now run `--release --emit vir`, report the
`verify` and `optimize` stages, and add `long_main_<4 × units>` (thousands of classes with an
override and a generic instance each, all used from one `main`): the shape that exposed
algorithms quadratic in the size of one function.

`pwsh bench/compile/run.ps1 -Runs 5` (best of 5, ms; "optimize" after also includes the VIR
verification that follows it):

| program | lines | lower | verify | optimize | lower | verify | optimize |
|---|---:|---:|---:|---:|---:|---:|---:|
| | | before | | | after | | |
| all_std | 19 | 0.6 | 0.1 | 31.9 | 0.6 | 0.1 | 9.6 |
| units_1000 | 62132 | 119.8 | 24.3 | 2156.2 | 83.6 | 30.3 | 1646.4 |
| chain_1000 | 62008 | 124.8 | 27.4 | 2063.8 | 76.4 | 27.7 | 1445.6 |
| long_main_4000 | 84008 | 323.2 | 705.6 | 21961.3 | 101.2 | 41.9 | 683.3 |

Scaling after (one run each): units 1000 → 4000: optimize 1.6 → 7.5 s, lower 84 → 472 ms;
long_main 4000 → 16000: optimize 0.75 → 3.2 s, verify 47 → 194 ms. Before, long_main_8000
spent 22 s in constfold alone and 0.9 s in the post-optimization verify.

What was super-linear, and the fix (all behaviour-preserving):
- **VIR verify, definite assignment** (`velt_vir::verify::init`): a dense forward must-analysis,
  blocks × locals bools cloned per block. Now each block is summarized once, a use is accepted
  when an assigning block dominates it (new `verify/dominators.rs`, Cooper–Harvey–Kennedy), and
  only the rest get one backward search per local.
- **constfold**: dense lattice states per block (blocks × tracked locals; above a cap it fell
  back to allocating that much per block anyway). States now hold only the locals live at the
  block's start (`constfold/liveness.rs`), evaluated in one reusable scratch state. Same facts,
  so the same folds; functions over the old cap are now analyzed globally too.
- **copyprop**: a fresh per-local table per block; now one table, reset per block.
- **const_fields**: a whole-body scan per address-holding local (now one scan for all), a linear
  search of the pointer facts per call argument (now a map lookup), copy chains resolved one
  pass over all candidates per link (now Kahn's order), and read-only params recomputed in
  rounds over the whole program (now a worklist over callers).
- **simplify_cfg**: a fresh `seen` array per block while threading jumps.
- **lowering**: `has_header` scanned every class per class hierarchy, every dyn call joined the
  modes of every impl of the interface, `find_impl` scanned all impls. Now one pass over the
  classes, a memo per (interface, slot), and an impl index per interface.

Full builds of units_2000 after (`--timings`): release 73 s, of which clang 68 s (−O3 on 87 MB
of IR) and optimize 3.4 s (sroa 1.1 s, constfold 0.7 s); debug 4.1 s, of which Cranelift 3.1 s.
Before: release optimize 6.4–11.6 s, lower 0.35–0.96 s. Remaining costs, not changed:
- clang dominates release builds and is linear (≈ 45 µs per IR line). `-O2` instead of `-O3`
  compiles units_1000 in 28 s instead of 37 s; not adopted without runtime measurements.
- Class vtables carry format / clone / drop glue whether or not the program formats or clones
  (≈ 15% of units' IR).
- Cranelift needs ≈ 2 GB to compile long_main_4000's `main` in a debug build, and long_main_16000
  runs out of memory there (the front end and the optimizer stay under 350 MB).

## Compile speed: `velt check`, debug links (compile-speed stream, 2026-10-01)

Machine: cloud VM, x86_64 Linux (Ubuntu 24.04, kernel 6.18), 4 × Intel Xeon @ 2.10 GHz, 15 GB;
GNU ld 2.42, LLD 18, no mold. `bench/compile/run.sh 5 1000 target/release/velt` (second table)
plus the link stage of `velt build -v examples/http_hello.vlt` (debug build, best of 5, ms):

| runtime the debug build links | before: static, GNU ld | static, lld | **shared** (new default) | nothing changed (link skipped) |
|---|---|---|---|---|
| checkout (`target/debug`: debug runtime, 432 MB `libvelt_rt.a` with DWARF) | 2404 | 363 | **28** | 0.1 |
| installed (`target/release`: 84 MB `libvelt_rt.a`, 12 MB `.so`) | 492 | 103 | **29** | 0.1 |

Edit → rebuild of http_hello in debug from a checkout: ≈ 2.5 s before, ≈ 80 ms now (parse 7,
sema 6, codegen + link 30). The shared link time is flat in program size (units_1000, 62k lines:
102 ms shared vs 139 ms static/lld, the object itself being several MB). Startup of a hello world
is the same either way (≈ 3 ms). The executables of debug builds also shrink from 172 MB (static,
debug runtime) to 34 KB.

| program | `velt check`: parse + sema | `velt check`: whole command | debug link: shared runtime | debug link: static runtime (lld) | rebuild, nothing changed: link |
|---|---|---|---|---|---|
| http_hello | 11.3 | 16.0 | 29.2 | 92.1 | 0.1 |
| all_std | 14.0 | 18.9 | 29.6 | 97.9 | 0.1 |
| units_1000 | 191.4 | 216.8 | 102.2 | 138.5 | 25.2 |

`velt check` on the example packages (release `velt`, package mode, wall time of the whole
command incl. process start, std and the prelude, best of 5): chat 24 ms, log-pipeline 24 ms,
notes-cli 25 ms, todo-api 30 ms (target < 100 ms).

## Codegen round: Cranelift memory, codegen units, -O level, clang 22 (2026-10-01)

Machine: cloud VM, x86_64 Linux (Ubuntu 24.04), 4 × Intel Xeon @ 2.10 GHz, 15 GB; clang 18.1.3
(Ubuntu) and clang 22.1.8 (conda-forge). Single runs unless stated; the VM was otherwise idle.

### Cranelift memory on huge functions (#42)

`cranelift_frontend` keeps a table indexed by block for every `Variable`, so its memory grew with
variables × blocks, and VIR names every temporary. Locals assigned exactly once now bypass
`Variable` (the assignment's SSA value is used directly; VIR's definite-assignment check makes it
dominate every reachable read). Debug build (`velt build`, Cranelift), peak RSS of the whole
command and the codegen stage:

| program | before: peak | before: codegen | after: peak | after: codegen |
|---|---:|---:|---:|---:|
| long_main_1000 | 429 MB | 3.9 s | 140 MB | 1.3 s |
| long_main_2000 | 1504 MB | 10.8 s | 218 MB | 2.8 s |
| long_main_4000 | 5636 MB | 35.9 s | 367 MB | 6.1 s |
| long_main_8000 | — | — | 645 MB | 11.3 s |
| long_main_16000 | out of memory | — | 1208 MB | 26.5 s |

Translating `main` itself dropped from 9.8 s to 0.09 s (long_main_2000); what remains is
Cranelift's own compile (register allocation is mildly super-linear: 0.18 s at 2000, 1.7 s at
8000) and the ≈ 56 000 other functions. `bench/compile/stress.sh` (`stress.ps1`) builds
long_main_16000 and fails above 2 GB (1206 MB here); the nightly workflow runs it on Linux and
Windows.

### Release builds: codegen units and the optimization level (#43)

`VELT_CODEGEN_UNITS=N` splits a program into N codegen units compiled by parallel clang
processes; without it a program is one unit (see the decision below). `velt build --release`
codegen stage (IR printing + clang), seconds:

| program | clang 18, 1 unit | clang 18, 4 units | clang 22, 1 unit | clang 22, 4 units |
|---|---:|---:|---:|---:|
| units_1000 | 68.8 | 21.3 | 58.3 | 18.0 |
| chain_1000 | 68.3 | 21.1 | 53.2 | 19.6 |
| units_2000 | 168.5 | 56.1 | 118.7 | 59.9 |
| long_main_4000 | 137.6 | 100.2 | 150.4 | — |

long_main_4000 is one 72 000-statement `main` plus small functions: its unit is the critical path.
Importing small callees into it (as for other units) made that unit take 218 s, so callers over
5 000 statements import nothing. `-O2` instead of `-O3`: units_1000 60.5 s in one unit, 18.7 s
in four (−12 %), smaller than what units give.

Run time, CPU seconds, best of 5 interleaved runs (`--release`; reduced sizes: binary-trees 18,
fannkuch 10, fasta 5M, k-nucleotide and reverse-complement on fasta 2M, mandelbrot 4000, n-body
1e7, pidigits 3000 / limbs 5000, regex-redux on fasta 500k, spectral-norm 3000; `bench/*.vlt`
as is). "4 units" forces `VELT_CODEGEN_UNITS=4` on these small programs (by default they are one
unit): the cost of losing cross-unit inlining.

| program | -O3 clang 18 | -O2 | -O3 clang 22 | -O3, 4 units |
|---|---:|---:|---:|---:|
| binary-trees | 0.856 | +3 % | +1 % | −8 % |
| binary-trees arena | 0.333 | 0 % | −1 % | −2 % |
| fannkuch-redux | 0.255 | +5 % | +2 % | −14 % |
| fasta | 0.793 | 0 % | +2 % | −1 % |
| k-nucleotide | 0.839 | +8 % | +4 % | +2 % |
| mandelbrot | 1.533 | +3 % | +2 % | 0 % |
| mandelbrot opt | 0.266 | −2 % | +28 % | −1 % |
| n-body | 0.701 | −3 % | −12 % | +23 % |
| n-body opt | 0.733 | 0 % | −2 % | +18 % |
| pidigits | 0.135 | −1 % | +3 % | +2 % |
| pidigits limbs | 0.561 | +6 % | −12 % | 0 % |
| regex-redux | 0.151 | −2 % | 0 % | 0 % |
| reverse-complement | 0.069 | −16 % | −9 % | −6 % |
| spectral-norm | 0.512 | −6 % | −5 % | 0 % |
| classes | 0.274 | +5 % | +2 % | +3 % |
| closures | 0.346 | +5 % | +2 % | +3 % |
| fib | 0.029 | 0 % | −4 % | 0 % |
| floats | 0.077 | +4 % | +3 % | 0 % |
| hashmap | 0.262 | +20 % | +10 % | +14 % |
| loops | 0.217 | −3 % | −4 % | −2 % |
| nbody | 0.255 | +33 % | −15 % | +34 % |
| shapes | 0.128 | +4 % | −2 % | −11 % |
| sort | 0.099 | +12 % | −2 % | +1 % |
| strings | 0.110 | +1 % | +9 % | +5 % |

Programs large enough to split: each program above plus the units_1000 code (called once from
`main`, so it is compiled), about 150 000 VIR statements, 1 unit against 4, CPU seconds, best of 7
interleaved runs:

| program | 1 unit | 4 units | change |
|---|---:|---:|---:|
| nbody | 0.211 | 0.211 | 0 % |
| hashmap | 0.253 | 0.254 | +1 % |
| classes | 0.293 | 0.281 | −4 % |
| closures | 0.334 | 0.341 | +2 % |
| sort | 0.085 | 0.087 | +3 % |
| strings | 0.114 | 0.116 | +1 % |
| shapes | 0.079 | 0.077 | −3 % |
| floats | 0.060 | 0.061 | +2 % |
| loops | 0.228 | 0.230 | +1 % |
| n-body | 0.515 | 0.510 | −1 % |
| spectral-norm | 0.462 | 0.457 | −1 % |
| binary-trees | 0.755 | 0.865 | +15 % |
| fannkuch-redux | 0.225 | 0.234 | +4 % |
| mandelbrot | 0.997 | 1.034 | +4 % |

Best of 15 again: binary-trees +13 %, fannkuch-redux +5 %, mandelbrot +1 %. binary-trees frees
each tree through drop glue that is generated per type and lands far from the class in program
order: `drop TreeNode` (first unit) → `objdrop TreeNode` → `drop Option<TreeNode>` (last unit) →
`drop TreeNode`, so the recursion crosses units at every node. Importing small callees
transitively (four levels) puts `available_externally` copies of all three in both units, but
LLVM still does not inline through the recursion (+15 % in a second run; one unit inlines the
whole chain into `objdrop`, internal functions with one caller). A second full run also moved
classes from −4 % to +10 % and hashmap from +1 % to +6 %: single runs on this cloud VM vary by
up to ±10 %.

The same comparison on Windows 11 (x86_64, 20 logical cores, clang 22.1.8; the units_1000 code
appended to each program and called first from `main`), CPU cycles of the process (10⁹,
`QueryProcessCycleTime`), best of 15 interleaved runs. The machine was fully loaded by other
builds throughout: max/min of the 15 runs of one binary is 31–80 %, and the best of a single
earlier round differed from these by up to 30 %, so changes below ±10 % are noise here:

| program | 1 unit | 4 units | change | median 1 unit | median 4 units |
|---|---:|---:|---:|---:|---:|
| binary-trees | 3.274 | 3.362 | +3 % | 3.985 | 4.205 |
| fannkuch-redux | 0.852 | 0.836 | −2 % | 1.002 | 1.037 |
| mandelbrot | 3.344 | 3.586 | +7 % | 4.118 | 4.034 |
| n-body | 1.865 | 1.697 | −9 % | 2.213 | 2.409 |
| nbody | 0.638 | 0.675 | +6 % | 0.866 | 0.897 |
| hashmap | 0.691 | 0.677 | −2 % | 0.949 | 1.009 |
| classes | 0.691 | 0.752 | +9 % | 0.860 | 0.850 |
| closures | 1.036 | 1.042 | +1 % | 1.225 | 1.186 |

This neither confirms nor rules out the Linux losses; the quiet-machine numbers above
(binary-trees +13–15 %) decide.

Decision: `-O3` stays (`-O2` is 12–33 % slower on nbody, hashmap and sort and saves ≈ 12 % of
clang time; `VELT_LLVM_OPT` selects another level). Splitting is opt-in (`VELT_CODEGEN_UNITS`),
because split programs can run up to 15 % slower: forced units cost up to a third on small
n-body-like programs, where the hot loop calls across the split, and recursive drop glue across
units costs binary-trees 13–15 % in a program split by size. #192 tracks what splitting by default
needs (since then: "Codegen units placement" below, which splits large programs by default).

### n-body with clang 22 (FINDINGS 8.9)

N = 50 000 000, CPU seconds, best of 5:

| implementation | CPU s | × Rust (generic) |
|---|---:|---:|
| Rust `n-body.rs` (rustc 1.97, LLVM 22, `-C target-cpu=native`) | 2.11 | 0.92 |
| Rust `n-body.rs` (same, no `target-cpu`) | 2.29 | 1.00 |
| Velt `main.vlt`, clang 18 | 3.41–3.60 | 1.49–1.57 |
| Velt `main.vlt`, clang 22 | 3.39–3.40 | 1.48 |
| Velt `main_opt.vlt`, clang 18 | 3.75 | 1.64 |
| Velt `main_opt.vlt`, clang 22 | 3.69 | 1.61 |

clang 22 compiles the same IR to code as fast as clang 18 (within noise) on x86_64: the newer
LLVM does not close the gap here.

## Codegen units placement (#190, #192, 2026-10-02)

Codegen units used to be contiguous runs of functions in program order. Now (#192) placement
follows the reference graph: its strongly connected components (iterative Tarjan) are walked
callers first; a component of up to 2 000 statements joins the group of its first referrer while
that group stays under half a unit's share; groups are ordered depth first (each after the group
of its first referrer) and cut into units of similar weight. Functions shared between units are
also declared `hidden` (`dso_local` for exported ones) in the units that call them (#190), so
cross-unit calls and addresses are direct, never through the PLT or GOT.

`bench/compile/split_runtime.sh` (new) builds composites: the units_1000 program of
`bench/compile/run.sh` (called once from `main`, so it is compiled) plus one benchmark, about
328 000 VIR statements and 62 000 lines each, at the reduced sizes of the codegen round above.
It checks that both builds print the same output, then times interleaved rounds alternating
which build runs first. CPU seconds (user + system), best of 9 rounds, `--release` (LLVM),
Apple M4 (10 cores), macOS, Apple clang 21. The machine is shared with other agents' builds:
the first runs, at load averages of 30–50, varied by up to ±10 % and are not shown; the runs
below were at load averages of 2–5. "change" is 4 units against 1 unit, "2nd run" a full
second run of the same comparison; "4 units, contiguous" is the earlier placement (with the
`hidden` declarations of #190); "8 units" the change against 1 unit of two more runs with 8
units.

| program | 1 unit | 4 units | change | 2nd run | 4 units, contiguous | placement vs contiguous | 8 units (two runs) |
|---|---:|---:|---:|---:|---:|---:|---:|
| classes | 0.107 | 0.107 | −0.0 % | +0.3 % | 0.106 | −0.2 % | +0.4 %, −0.1 % |
| closures | 0.186 | 0.188 | +0.7 % | +0.5 % | 0.184 | +0.8 % | +1.8 %, −0.9 % |
| fib | 0.019 | 0.018 | −1.4 % | +0.5 % | 0.019 | −1.2 % | +7.6 %, +5.1 % |
| floats | 0.037 | 0.037 | +0.8 % | +0.2 % | 0.037 | −0.2 % | +0.5 %, +0.6 % |
| hashmap | 0.072 | 0.072 | +0.1 % | −0.2 % | 0.077 | −7.0 % | −0.0 %, −0.2 % |
| loops | 0.149 | 0.149 | +0.2 % | +0.1 % | 0.148 | −0.0 % | +0.2 %, −0.0 % |
| nbody | 0.108 | 0.108 | +0.1 % | +0.0 % | 0.175 | −38.6 % | +0.1 %, +0.0 % |
| shapes | 0.032 | 0.033 | +1.4 % | +0.7 % | 0.032 | +0.1 % | +0.0 %, +0.1 % |
| sort | 0.048 | 0.047 | −0.2 % | +0.4 % | 0.047 | −0.4 % | +0.6 %, +0.3 % |
| strings | 0.049 | 0.050 | +1.3 % | +1.6 % | 0.049 | +1.8 % | −0.7 %, −0.8 % |
| binary-trees | 0.368 | 0.362 | −1.5 % | −2.3 % | 0.371 | −2.4 % | +6.0 %, +5.4 % |
| binary-trees arena | 0.157 | 0.143 | −8.7 % | −8.8 % | 0.142 | +0.0 % | −9.5 %, −9.0 % |
| fannkuch-redux | 0.134 | 0.134 | +0.1 % | +0.1 % | 0.133 | −0.4 % | +0.2 %, −0.2 % |
| fasta | 0.444 | 0.447 | +0.5 % | −0.4 % | 0.440 | +0.8 % | −0.2 %, +0.2 % |
| k-nucleotide | 0.318 | 0.315 | −1.0 % | +0.3 % | 0.320 | −0.7 % | −0.6 %, −0.6 % |
| mandelbrot | 0.542 | 0.542 | −0.1 % | +0.0 % | 0.540 | −0.0 % | −0.1 %, −0.1 % |
| mandelbrot opt | 0.136 | 0.137 | +0.1 % | +0.1 % | 0.136 | −0.0 % | +0.2 %, +0.1 % |
| n-body | 0.321 | 0.322 | +0.2 % | +0.2 % | 0.364 | −12.0 % | −0.1 %, +0.0 % |
| n-body opt | 0.364 | 0.364 | +0.1 % | +0.1 % | 0.410 | −11.4 % | +0.2 %, −0.1 % |
| pidigits | 0.065 | 0.065 | +0.3 % | +0.5 % | 0.065 | −0.2 % | +0.2 %, +0.3 % |
| pidigits limbs | 0.300 | 0.301 | +0.1 % | +0.1 % | 0.300 | +0.1 % | −0.0 %, −0.0 % |
| regex-redux | 0.054 | 0.054 | −0.7 % | +0.0 % | 0.053 | +0.4 % | +0.6 %, +0.9 % |
| reverse-complement | 0.022 | 0.023 | +0.6 % | +1.1 % | 0.023 | −0.2 % | +0.4 %, +0.4 % |
| spectral-norm | 0.205 | 0.205 | +0.0 % | +0.0 % | 0.192 | +6.6 % | +0.2 %, +0.1 % |

- In 4 units every composite runs within ±3 % of one unit in both runs, except binary-trees
  arena, which is 9 % *faster* split (also in 8 units). binary-trees' recursive drop glue now
  stays in one unit (`drop TreeNode` → `objdrop TreeNode` → `drop Option<TreeNode>`); on this
  machine the contiguous split cost it only 2 % (13–15 % on the Linux VM of the codegen round).
- Against the contiguous split: nbody −39 %, n-body −12 %, n-body opt −11 %, hashmap −7 %: their
  hot loops no longer call across units. spectral-norm +7 % is the contiguous split being faster
  than one unit; the placed split equals one unit.
- With a first threshold of 500 statements, k-nucleotide ran 12 % slower in 4 units (in two
  runs): `frequencies` (525 statements once `velt_opt` inlines `Map.upsert` into it) started a
  group of its own, placed after the whole units_1000 code, so its hot calls to `Map.lookup`
  (57 statements, too large to import) crossed units. The threshold is now 2 000, and groups
  are ordered after their caller's group.
- In 8 units, fib and binary-trees run 5–8 % slower in both runs although their hot functions
  are in the same unit as in 4: fib's machine code is byte for byte the same, at another address
  (code alignment). The default therefore stops at 4 units.

Codegen stage of 4-unit builds (`VELT_CODEGEN_UNITS=4`, seconds, best of 2): the placement is
not slower to compile than the contiguous split.

| program | contiguous | placed |
|---|---:|---:|
| units_1000 | 3.41 | 2.69 |
| chain_1000 | 3.62 | 2.82 |
| long_main_4000 | 7.86 | 7.80 |

Composites in one unit take 8.5 s of codegen, 2.6 s in 4 units and 2.0 s in 8.

Decision: release builds of large programs are split by default. The unit count depends on the
program's size only (so objects do not depend on the machine): one unit below 32 000 VIR
statements (every benchmark program is one unit; the largest, `bench/sort.vlt`, has about
4 000), else one per 16 000, at most 4; clang runs at most one process per core.
`VELT_CODEGEN_UNITS=N` overrides the count, `=1` turns splitting off.

## Parse time: lexing on demand (#136, 2026-10-02)

`cargo test --release -p velt_syntax --test bench -- --ignored --nocapture` (parse only, the AST
drop not counted), on Windows 11, x86_64, 20 logical cores, a busy machine. Pre-#118 is the commit
before JSX moved to the parser; the benchmark file was copied onto each commit and run on the same
files (`VELT_BENCH_ROOT`). Best of 15 interleaved runs, ms (median in brackets):

| input | pre-#118 | main before #136 | after #136 |
|---|---|---|---|
| no JSX, 100k lines | 271.3 (279.4) | 292.0 (302.3) | 275.1 (280.8) |
| std and examples (119 files) | 23.5 (25.1) | 25.9 (26.9) | 24.3 (25.9) |
| JSX and generics, 50k lines | 57.6 (60.2) | 69.1 (71.0) | 60.7 (64.9) |
| JSX goldens (25 files) | 3.51 (3.65) | 3.88 (3.97) | 3.63 (3.79) |

Wall times on that machine vary by about ±4% between identical runs, so instruction counts
(callgrind, WSL, 20k lines no JSX / 10k lines JSX / std and examples, whole test process) are the
sharper comparison: no JSX 353.7M → 388.3M → 352.4M, JSX 80.8M → 91.4M → 83.1M, std and examples
88.5M → 96.0M → 88.6M.

## `velt dev` debug-info cost (#191, 2026-10-02)

Each JIT version registers an in-memory ELF image with DWARF line tables for debuggers (GDB JIT
interface). The first version holds every function, std included. Release `velt dev --host
--timings` on an Apple M4 (10 cores, macOS 26) shared with other builds; with and without
`VELT_DEV_DEBUG_INFO=0`, 15 interleaved runs each, best (median in brackets), ms. Peak RSS from
`/usr/bin/time -l`. log-pipeline is `examples/apps/log-pipeline` running `src/main.vlt -- gen
x.log --lines 10`. units_1000 is the 1000-unit program of `bench/compile/run.sh`.

| program | functions (image) | `debug info` step | `jit` stage, on / off | time to first run, on / off | peak RSS, on / off |
|---|---|---|---|---|---|
| log-pipeline | 306 (81 KB) | 0.6 (0.7) | 48.5 / 46.6 | 74 (95) / 78 (95) | 28.2 / 27.7 MB |
| units_1000 | 22,385 (2.9 MB) | 15.7 (24.7) | 855 / 865 | 1241 (1727) / 1134 (2223) | 224.6 / 205.6 MB |

The image takes under 2% of the time to the first run, well inside the noise between runs, and
less than 50 ms. Its memory is a peak of 9% on the large program (gimli's tables and the
image), 2% on log-pipeline. Neither crosses the thresholds set in #191 (5% or 50 ms of startup,
10% of peak RSS), so the image is still built on the startup path. `VELT_DEV_DEBUG_INFO=0`
turns it off.

## JSON (#228, #229, 2026-10-02)

`bench/json/run.sh 5`: each program in bench/json/ (Velt LLVM release), a Rust version with
serde (derive) and serde_json (`preserve_order`, so objects keep insertion order like JavaScript
and Velt), and a Node version; the harness checks that all print the same output. Apple M4
(4 performance + 6 efficiency cores), macOS 26.6, rustc 1.99.0, Node 24.11.1. The machine was
shared with other builds (load average 40–90), so the numbers vary by ±20% or more between runs;
compare columns of the same run only.

| benchmark | Velt (LLVM release) | Rust serde_json | Node |
|---|---|---|---|
| edit | 11 | 556 | 29 |
| navigate | 160 | 240 | 73 |
| parse_typed | 257 | 303 | 258 |
| parse_union | 205 | 143 | 244 |
| parse_value | 408 | 465 | 264 |
| stringify | 119 | 69 | 252 |

- **parse_typed**: `JSON.parse<Item[]>` of 100k objects (8 MB), 8 times (Node: `JSON.parse`).
- **parse_union**: `JSON.parse<Shape[]>` of 200k objects of a two-member union, the
  discriminant first in one member and last in the other, 8 times (Rust: an internally tagged
  enum).
- **stringify**: `JSON.stringify` of 100k structs, 8 times.
- **parse_value**: `JSON.parseValue` of the parse_typed document, 8 times.
- **navigate**: `at(i)` and `get(key)` over the parsed document, 20 passes.
- **edit**: `set` 10k keys into an object, then `delete` them all from the front, 4 times
  (Rust: `shift_remove`, which keeps the order and moves the later members, O(n) per delete).

Before and after #228 (interleaved runs of both builds, best of 7, ms): parse_typed 297 → 292,
parse_union 458 → 430, parse_value 654 → 609, navigate 299 → 293, stringify 159 → 157, edit
537 → 23. Deleting from the front was O(n) per delete and is now O(1) amortized; `json.Value`
nodes shrank from 80 to 40 bytes (an object's key index moved behind a box), which makes
`parseValue` a little faster.

Positions after deletes in the middle (Fenwick tree of the live slots, 2026-10-03; interleaved
runs, best of 21, CPU ms): parse_value 313 → 315, navigate 170 → 174, edit 10 → 10, the rest
unchanged (within ±2%). Deleting 80k keys from the middle of a 160k-key object, each followed
by two `at` calls: 9.9 s → 0.04 s (each `at` rebuilt a table of the live positions in O(n);
it now costs O(log n)).

## Generators (`bench/iter`, #62 phase 2, 2026-10-03)

`bench/iter/run.sh` (`run.ps1`): 20 × 30M values summed modulo a prime (a loop-carried
dependency the vectorizer leaves alone), LLVM release, best of interleaved runs, Apple M4 shared
with other builds (identical programs vary by up to ±10% between rounds). Node runs once.

| program | Velt (ms) | vs hand_loop | Node (ms) |
|---|---|---|---|
| hand_loop: `while` loop | 1172 | 1.00 | 2649 |
| gen_loop: `for...of` over `range(n)`, a `function*` | 1233 | 1.05 | 77980 |
| iterable_class: iterator class (`next()` called directly) | 1147 | 0.98 | 17341 |
| gen_value: `range(n)` passed as `Iterable<i64>` | 2997 | 2.56 | 74180 |

- gen_loop's hot loop is the hand loop's seven instructions per value (rotated): the generator's
  state lives in the loop's frame, its resume function is inlined, and the state's dispatch is
  jump-threaded away; nothing is allocated. Where the hand loop vectorizes (a filtered plain
  sum), the generator loop stays scalar: the `yield` is an exit from the producer's loop.
- gen_value allocates the generator object once per loop; each value costs an interface call of
  `next()`, a table call of the resume function and an `IteratorResult` value.
- Array `for...of` is untouched: bench/classes, shapes and nbody lower to identical VIR before
  and after this change, hashmap differs only in the source paths of panic messages; their
  times are unchanged within noise.

## Async generators (`bench/iter`, #62 phase 3, 2026-10-03)

`bench/iter/run.sh 7` adds an async pair: 20 × 3M values, each from an async call that
completes at once (`await step(i)`), summed like the rest; compared with the async hand loop.
Same machine and caveats as above (this run's sync rows: hand_loop 456 ms, gen_loop 454,
iterable_class 455, gen_value 1125 — the machine was quieter than in the phase-2 run).

| program | Velt (ms) | vs async_hand | Node (ms) |
|---|---|---|---|
| async_hand: `while` loop in an async function | 49 | 1.00 | 1876 |
| async_gen: `for await` over `values(n)`, an `async function*` | 118 | 2.41 | 7043 |

- Nothing is allocated in either: the async generator's state is part of `main`'s state and is
  polled by a direct call per value.
- The difference (about 1.2 ns per value) is the generator's state living in the caller's
  state memory rather than registers: each step stores and reloads its tag and counter and
  tests the poll result. Real work per value (an actual suspension or I/O) dwarfs it.

## Channels as async iterables (#62 phase 4, 2026-10-03)

`for await (const v of ch)` against the `await ch.receive()` loop it replaces, same build (LLVM
release, interleaved, best of 21, Apple M4 shared with other builds, `VELT_THREADS=1`):

| program | `receive()` loop (ms) | `for await` (ms) |
|---|---|---|
| bench/async channel_pipeline (4 producers, 1M values, capacity 1024) | 44.6 | 46.8 |
| drain: 10 × 1M values queued, then received in one task | 432.6 | 428.6 |

- Nothing is allocated per value: the channel's `[Symbol.asyncIterator]` is an async generator
  method, so the loop embeds its state and calls the same runtime receive. The pipeline's
  ~2 ns per value is the generator step (section "Async generators" above).
- Every bench/async and bench/iter program compiles to a byte-identical object file before and
  after this phase (it changes std sources and sema only), so their timings are unchanged.

## JS int32 operators (#521, 2026-10-04)

Bitwise operators on numbers now follow JS: ToInt32 and ToUint32 of their operands, and products
inside them rounded like doubles. Before, `(y * k) | 0` computed `Math.imul`'s value and `x >>> 15`
shifted 64 bits. ToInt32 of a float was a `trunc` call plus an `fmod` call (`__toInt32`); it is now
one guarded `cvttsd2si`. `numrep` (velt_opt) keeps `number` locals that only hold int32 values in
`i32` registers.

The issue's repro, `velt_perf_repro`, ran 20M iterations × 1 sample per run. Each cell is the
best of 8–12 interleaved runs in ms (Windows, i9-12900HK, with other builds running, so ±30%):

| | Velt release before | Velt release after | `velt run` before → after | Rust -O | Node |
|---|---|---|---|---|---|
| as written: `(y * k) \| 0` | 2122–2331 | 279–370 | 3800 → 946 | 95–114 | 228–372 |
| with `Math.imul(y, k)` | n/a (no `Math.imul`) | 115–133 | | 95–114 (same instructions) | |

- **Checksums.** For 100M × 5, Velt now prints Node's `-566856265`; before, it printed
  `-420279296`. With `Math.imul`, Velt prints Rust's `2105069163`, as Node does for that program.
- **The loop as written.** It is about 3× Rust, and in the noise of Node's time.
  - The rest of the gap is the rounding JS requires: an `i64` product, `cvtsi2sd` and `cvttsd2si`
    on the dependency chain of each multiply, about 16 cycles. V8 emits `vcvtlsi2sd`, `vmulsd` and
    `vcvttsd2siq` for it, which has the same latency.
  - Node was 6.5× Rust in the issue author's measurement but 2.4× Rust on this machine. On a
    machine where Node is slower, Velt is faster.
- **The `Math.imul` loop** compiles to the instructions Rust emits: LLVM even unrolls it by two.

`bench/int32` (both variants, whole process, best of 5 interleaved, busy machine): Rust 2763 ms,
Velt LLVM 3732, Velt Cranelift release 6395, Node 7360.

## Typical TypeScript workloads (`bench/typical`, 2026-10-04)

`bench/typical/run.sh 7`: 15 programs written the way TypeScript code is written (`number`
parameters, classes, closures, `Map<string, number>`, comparator sorts, string building). Each
`.vlt` file is also the TypeScript program Node runs (the harness appends the `main();` call),
and `rust/<name>.rs` is an idiomatic Rust port that allocates where JavaScript does (`Box` per
class instance, `Box<dyn Fn>` for stored closures); `vec2.rs` uses a `Copy` struct, what a Rust
programmer writes there. All three print the same output. Best of 7 interleaved runs, wall ms;
Windows 11, i9-12900HK shared with other builds, clang 22.1.8, rustc 1.99 `-O`, Node 22.22.

| benchmark | Velt (LLVM release) | Rust -O | Node | Velt / Rust | Velt / Node |
|---|---:|---:|---:|---:|---:|
| grid | 499 | 131 | 856 | 3.81 | 0.58 |
| vec2 | 382 | 104 | 950 | 3.67 | 0.40 |
| tokenize | 848 | 328 | 477 | 2.59 | 1.78 |
| sortcmp | 1097 | 473 | 2807 | 2.32 | 0.39 |
| record | 322 | 198 | 622 | 1.63 | 0.52 |
| errors | 217 | 175 | 851 | 1.24 | 0.25 |
| graph | 340 | 302 | 903 | 1.13 | 0.38 |
| fib | 120 | 107 | 377 | 1.12 | 0.32 |
| numloop | 267 | 249 | 893 | 1.07 | 0.30 |
| wordcount | 542 | 514 | 1386 | 1.05 | 0.39 |
| chains | 333 | 329 | 2732 | 1.01 | 0.12 |
| callbacks | 168 | 203 | 1108 | 0.83 | 0.15 |
| strings | 1583 | 2072 | 9439 | 0.76 | 0.17 |
| objects | 307 | 545 | 1738 | 0.56 | 0.18 |
| keys | 262 | 611 | 833 | 0.43 | 0.31 |

The four large gaps have one cause each (survey #529 has the evidence and the proposed fixes;
the table is from `main` at 8949f2c7):
- **grid**: `%` on `number` is a libm `fmod` call, about 10 ns (`n: number` in the Game of Life;
  with `n: i64` it is 5.7× faster). #532.
- **vec2**: every `new Vec2(…)` of the loop-carried value is a heap allocation and a free. #533.
- **tokenize**: `src[i]`, `c >= "0"` and `c === " "` are runtime calls on one-character
  strings (the same loop with `charCodeAt` is 5× faster); `record`'s `includes` / `indexOf` on
  `string[]` pay a `velt_rt_str_eq` call per element. #531.
- **sortcmp**: `sort(cmp)` merges in place with SymMerge, O(n log² n) swaps. #530.

## Callback arguments of arrays other references reach (#564, 2026-10-06)

A call argument borrowed from an element of a boxed array (or one reached through a counted
object or a cell) is now a share of the element whenever the array's count says another
reference exists, so a callback that pushes onto the array or pops it cannot leave it dangling.
Wall clock, Windows 11, i9-12900HK shared with other builds, LLVM release, best of 11: 20,000
rounds of `forEach` plus `map` over 1,000 elements (40M callback calls); before is `main`'s
lowering and std.

| elements | before | after |
|---|---:|---:|
| `number[]` | 82 ms | 87 ms |
| objects (`P[]`) | 81 ms | 58 ms |
| `string[]`, type counted (an alias exists elsewhere), this array unaliased | 109 ms | 83 ms |
| `string[]` with a live alias (`const alias = shared`) | 116 ms | 739 ms |
| `string[]` in a program without aliases | 49 ms | 47 ms |

Only the last-but-one row pays: one string share and drop per call (about 15 ns, two atomic
count updates). The others are within noise: an unshared type keeps its code, and an array of a
counted type checks its count (a load and a branch) before borrowing in place.
