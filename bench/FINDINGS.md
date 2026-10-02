# Performance findings for other owners

Gaps to Rust found with the M2 benchmarks that the backends and `velt_opt` cannot close
alone, because they come from lowering (`crates/velt_vir/src/lower`), the prelude
(`std/prelude`) or a missing language/contract guarantee. Each item shows the VIR (or IR)
and the proposed change. Numbers are best-of-7 wall-clock ms on the machine described in
RESULTS.md; VIR snippets are from `velt build --release --emit vir bench/<name>.vlt`.

## 1. Vtables are dispatcher functions: two indirect calls per virtual call (lowering)

Owner: `lower/glue/vtable.rs`, `lower/callee.rs` (`dispatch`). Affects `classes`, interface
values (`Scorer[]`), and every virtual `drop` / `clone` / `print`.

A vtable is a function `(slot: i64) -> ptr` that switches on the slot, so `s.area()` is one
call to find the entry, then a call to the entry:

```
    _4 = (*_40 as ptr)                        // the object
    _42 = (*_4 as agg#6).0                    // its vtable = a dispatcher function
    _43 = call (_42: fn(i64) -> ptr)(0_i64)   // indirect call 1: which `area`?
    _44 = call (_43: fn(ptr) -> f64)(_4)      // indirect call 2: the method
fn#1 internal _Gvtable_33(i64) -> ptr {
  bb0: switch _0 [0 => bb2, -1 => bb3, -2 => bb4, -3 => bb5], default bb1
  bb2: return fn#7   ...
```

The header comment of `vtable.rs` says VIR statics have no relocations; they do now
(`StaticData::relocs`, supported and tested in both backends). Proposed:
- Emit each vtable as a static of 8-byte slots with
  `relocs: [(8 * (slot + K), Const::Func(entry))]`, where `K` is the number of negative slots
  (drop / clone / print), so every slot has a fixed non-negative offset.
- Store `Const::Static(vtable)` in the object header / fat pointer instead of `Const::Func`.
- `dispatch(vtable, slot)` becomes `p = ptradd vtable, 8 * (slot + K); f = (*p as ptr)`: one
  load, then the single indirect call.

Velt `classes` already beats Rust only because Rust's version allocates a `Box` per shape; the
dispatch loop itself does twice the calls of Rust's `call [vtable + 8*k]`.

## 2. Non-escaping closures with only Copy captures get a heap environment (lowering)

Owner: `lower/closure.rs` (`closure_env_is_heap`). Affects `closures` (every `map` / `filter`
callback that captures a number).

`xs.map((x) => x * k)` captures `k` by copy and is passed directly as an argument
(non-escaping), yet its environment is heap-allocated and later dropped through its header:

```
    _17 = call extern#0 velt_rt_alloc(24_u64, 8_u64) -> bb10
    (*_17 as agg#3).0 = fn#2           // env drop
    (*_17 as agg#3).1 = fn#3           // env clone
    (*_17 as agg#3).2 = _k
    _23 = agg#2 { fn#1, _17 }
    ...                                // after the call:
    _97 = (*_18 as agg#3).0
    call (_97: fn(ptr) -> unit)(_18)   // drop through the header, then velt_rt_free
```

`closure_env_is_heap` returns true when no capture is a borrow
(`!caps.is_empty() && (owns || !borrows)`): it decides by capture *mode*, not by whether the
closure escapes. Proposed: decide by escape. A closure sema marks non-escaping (a direct
argument or immediately called) gets a stack env with null drop/clone, like borrowing
closures, whatever its capture modes (Copy captures are plain values in the env; Owned
captures get dropped at scope end, as the captured variables would have been). With a stack
env, `velt_opt` (`addr_forward` + `sroa`) turns the captures into registers after inlining,
as it already does for borrowing closures.

## 3. ~~No exclusivity for `mut` borrows: array headers are reloaded after every store~~ (done)

Done: sema enforces exclusive `mut` access; `vir::Function::param_attrs` (vir.rs invariant 9)
carries `noalias` / `readonly` / `nonnull` / `dereferenceable`, emitted as LLVM parameter
attributes; `velt_opt/src/noalias.rs` keeps the scalar fields behind a `noalias` param in
locals. `noalias` alone was not enough for LLVM: the data pointer is loaded from memory and
the array pointer escapes into the recursive calls, so LLVM still reloaded `xs.data` after
every element store; the velt_opt promotion fixes that. What is left in the Lomuto loop is the
pivot `xs[lo]`, reloaded per iteration because element stores may hit it (the prelude could
hold it in a local), and one bounds check on `lt`. Original analysis:

Owner: sema (borrow rules) and the VIR contract (`vir.rs`). Affects `sort`,
`nbody` and any loop that writes array elements through a borrowed array.

`insertionSort` (inlined into the specialized `introsort`) after `velt_opt` and `clang -O3`:

```llvm
bb16:
  %t96  = load i64, ptr %t95, align 8      ; xs.length, reloaded every iteration
  %t99  = icmp ult i64 %l18, %t96          ; so the bounds check stays in the loop
bb18:
  %t108 = load ptr, ptr %p0, align 8       ; xs.data, reloaded every iteration
bb20:
  store i64 %t161, ptr %t110, align 8      ; element stores: LLVM must assume they may
  store i64 %t112, ptr %t157, align 8      ; overwrite the array header behind %p0
```

Rust passes `&mut [T]` / `&mut Vec<T>` as `noalias`, so pointer and length stay in registers
and bounds checks leave the loop. Velt cannot: sema does not reject `f(mut a, a)` (the same
value borrowed twice in one call, once mutably), so `noalias` on VIR `Ptr` params would be
unsound today. Proposed:
1. Sema: enforce Rust's rule for calls. A value passed as a `mut` (BorrowMut) argument may not
   also be passed, or captured by a closure argument, in the same call.
2. VIR contract: a per-param attribute on `Function`, e.g.
   `pub param_attrs: Vec<ParamAttr>` with `ParamAttr { noalias: bool, readonly: bool }`,
   set by lowering from `PassMode` (`BorrowMut` → noalias; `Borrow` of a non-`shared` value →
   noalias + readonly).
3. Backends (mine, once the attribute exists): LLVM `noalias` / `readonly` on those params;
   `velt_opt` can then hoist header loads and bounds checks itself for Cranelift.

## 4. ~~Integer `sort()` is a textbook introsort~~ (prelude, done)

Owner: `std/prelude/sort.vlt`. `sort()` is now pdqsort with branchless Lomuto partitioning:
1M i64 88 → 32 ms, 200k strings 43 → 30 ms (Rust `sort_unstable`: 15 / 23 ms). Details and
what was tried in RESULTS.md ("Prelude work"). The remaining integer gap is item 3 (header
reloads and bounds checks in the partition loop) plus the lack of a generic move out of an
array: elements can only be swapped, so the pivot and the element in flight cannot stay in
registers. An intrinsic such as `__intrinsic_array_get_unchecked` / a move-out-and-back pair,
or `noalias` from item 3, would let the partition loop reach Rust's shape.

## 5. String `==` calls the three-way `velt_rt_str_cmp` (lowering)

Owner: string comparison in `lower/ops.rs`. Affects `hashmap` (`Map<string, V>` probing) and
any string equality in a loop.

```
    _23 = ptradd (*_0 as agg#4).1.0, _22   // entryKeys[i]
    _24 = call extern#13 velt_rt_str_cmp(_23, _1)
```

rt_abi_async.md defines `velt_rt_str_eq` (length check plus one `memcmp`) for exactly this.
Proposed: lower `==` / `!=` on strings to `velt_rt_str_eq` and keep `velt_rt_str_cmp` for
`<`, `<=`, `>`, `>=`. Both are now declared read-only to LLVM (`memory(read)`), so either can
be hoisted or CSE'd, but `str_eq` rejects keys of different lengths without touching bytes.

## 6. `charCodeAt` is an opaque runtime call per byte (prelude / lowering)

Owner: `std/prelude/string.vlt` (`charCodeAt` → `velt_rt_str_char_code_at`). Affects
`strings` (25M calls in its scan loop).

Proposed: an intrinsic `__intrinsic_str_byte_at(s, i): i64` lowered inline to a bounds check
plus a `u8` load, returning -1 when out of range: the runtime function's semantics, but
inlinable and vectorizable. Low priority: `strings` is at parity with Rust, whose `format!`
is slower than Velt template literals.

## 7. ~~`Promise.all` over an array is quadratic inside a tokio worker~~ (runtime, fixed)

Fixed in `crates/velt_rt/src/task/all.rs` (no ABI change; details under "Fix" below):
`spawn_many` now takes 0.61 s on all cores and 0.70 s with `VELT_THREADS=1` (was 220 s /
330 s in the same session), `timers` 70 / 102 ms (was 1.2 / 3.2 s); see RESULTS.md "Async".
Regression tests: `task::all::tests` (100k join handles and 100k sleeps on a current-thread
runtime, fairness) and `abi_tests::core::all_over_100k_*`.

Owner: `crates/velt_rt/src/task/all.rs` (`velt_rt_all_with_drop`). Affects every
`Promise.all(array)` whose children consume tokio's cooperative budget: join handles
(`spawn`), `sleep`, and every fs / net / http leaf. Found with the async benchmarks
(RESULTS.md "Async"): `spawn_many` takes 77 s on all cores and 308 s with `VELT_THREADS=1`
(tokio: 0.5–0.8 s), `timers` 1.8–3.3 s (tokio multi-thread: 70 ms). Awaiting the same 1M join
handles one by one takes 0.44–0.49 s, so spawning, the task cells and `yieldNow` are fine.

The VIR is what one expects: one `velt_rt_spawn` (or `velt_rt_sleep` + `velt_rt_fut_box`) per
child, then a single `velt_rt_all(futs, n, 8, results)`. The time goes into the runtime:

```
    _18 = call extern#3 velt_rt_all(_43, _44, 8_u64, _15)
    _23 = call extern#5 velt_rt_fut_box(fn#4, fn#5, _22, 32_u64, 8_u64)   // the Promise.all state
```

Mechanism (tokio 1.53, futures-util 0.3.34):
1. `velt_rt_block_on` runs `async main` as a task on a worker, so every poll of the join runs
   with a coop budget of 128.
2. A `JoinHandle` / `Sleep` poll with the budget exhausted returns `Pending` and registers its
   waker with `context::defer` (`tokio/src/task/coop/mod.rs`, `register_waker`): the wake happens
   after the task yields, not during the poll.
3. `FuturesUnordered::poll_next` only yields early when a child woke itself *during* its poll
   (`yielded >= 2`); a deferred wake is not seen, so it keeps going until `polled == len`.
4. So once 128 children are done, every further poll of the join polls *every* remaining child
   (each returning `Pending`, each pushing a cloned waker into the defer list) to make
   progress on at most 128: O(n²/128) child polls, plus up to n deferred wakers held at once
   (part of `spawn_many`'s extra memory: 450–520 B per task against 310 B when awaited in turn).

It does not show up in the Rust multi-thread baseline only because `Runtime::block_on` runs the
root future on the calling (non-worker) thread, where `defer` wakes at once; the same idiomatic
`join_all` of 100k `sleep`s takes 3.3 s on tokio's current-thread runtime.
`bench/async/rust/src/bin/repro_all_budget.rs` reproduces both cases inside a task:

| join inside a task (multi-thread runtime) | n = 25k | 50k | 100k |
|---|---|---|---|
| `FuturesUnordered` of `JoinHandle`s (what `velt_rt_all` does) | 191 ms | 437 ms | 1630 ms |
| the same inside `tokio::task::unconstrained` | 14 ms | 27 ms | 56 ms |
| the same, budgeted (proposed fix below) | 15 ms | 35 ms | 71 ms |
| `FuturesUnordered` of `sleep(1ms)` | 119 ms | 369 ms | 1536 ms |
| the same inside `tokio::task::unconstrained` | 20 ms | 27 ms | 50 ms |
| the same, budgeted | 13 ms | 35 ms | 49 ms |

(Current-thread runtime: 3.3–3.4 s for 100k unbudgeted, 54–81 ms with either fix.)

Originally proposed: the join pays the budget instead of the children. In
the `velt_rt_all` leaf, poll the set with `tokio::task::unconstrained(set.next())` and call
`tokio::task::consume_budget().await` after each finished child (`drain_budgeted` in the
repro). Children are then always polled with budget, so none returns a spurious `Pending`,
and the join still yields to other tasks every 128 results. Plain `unconstrained` around the
whole join is as fast but never yields, so one large `Promise.all` could starve the worker's
other tasks. Checking `has_budget_remaining()` before each `set.next()` is not enough: the
budget runs out in the middle of a `poll_next`, which then still scans every pending child
once, so each 128 results cost a full scan and the join stays quadratic. A regression test:
`Promise.all` over 200k spawned handles that each `yieldNow()` once should finish in well
under a second (`bench/async/spawn_many.vlt` is the 1M version).

Fix (as implemented, `gated` in the repro): children keep their budget instead of running
unconstrained, because a child is arbitrary compiled code: a child looping over an always-ready
channel would never yield inside `unconstrained`. Instead a child is never *polled* once the
budget is spent: its `poll` checks `has_budget_remaining()` and, when empty, wakes itself and
returns `Pending` without touching the `VeltFut`. `FuturesUnordered` treats two such self-wakes
as a yield request and returns, so an exhausted budget costs two no-op child polls instead of a
scan of every pending child (the per-child check is what the per-`set.next()` check above was
missing). The join also calls `consume_budget()` once per finished child, so children that
never touch the budget still let other tasks run every 128 results; when that payment spends
the budget it yields at once through `yield_now` (a deferred wake: tasks that yielded
themselves only run when the scheduler checks for events, so a direct self-wake would put the
join ahead of them for up to 61 ticks). In the repro the gated join is as fast as `budgeted`
(this session, loaded machine, 100k: 147–187 ms handles, 105–138 ms sleeps, against
11–14 s unfixed).

## 8. Benchmarks Game: gaps over 1.2× vs Rust

The numbers are from `bench/benchmarks-game/RESULTS.md`: Apple M4, CPU seconds against the
single-threaded Rust program, Velt `--release` with LLVM. Each program's `NOTES.md` has the full
analysis with the VIR, IR and assembly. Gaps that come from the program's algorithm are listed
last, because no compiler change closes them.

### 8.1 ~~Class-typed parameters other than `this` get no `noalias` / `readonly`~~ (lowering, done)
Owner: `crates/velt_vir/src/lower/abi.rs`. Affects fasta (≈15%), pidigits (≈10%), and every loop
that modifies one object while reading another.

Only `this` is classified as `PtrParam::Object`. Other class-typed params fall into `NotPtr` and
get `ParamAttrs::default()`:

```llvm
define internal void @"_V8AlphabetM4next"(ptr readonly nonnull dereferenceable(48) %p0,
    ptr %p1, i64 %p2, ptr noalias nonnull dereferenceable(24) %p3)   ; %p1 = rng: Random
```

As a result, fasta loads and stores `rng.seed` on every number, inside the LCG's loop-carried
dependency. pidigits reloads `other.limbs.ptr` and `other.limbs.length` on every limb. Proposed
fix: classify every class-typed param as `PtrParam::Object`, so that `BorrowMut` params get
`noalias` and `Borrow` params get `readonly`. The exclusivity rule already guarantees both, and
`docs/internals/contracts/README.md` already promises `readonly` for read-only params.

**Done** (perf round 3): every class-typed param is `PtrParam::Object`; the soundness argument
(unique ownership, the exclusivity check covering aliases through element bindings and unknown
callees, no references returned into arguments) is in `crates/velt_vir/src/lower/param_attrs.rs`.
fasta's `next` now takes `ptr noalias nonnull dereferenceable(8) %p1`, and LLVM moves the
`rng.seed` load / store out of the per-number loop (one load on entry, one store on exit). On
the shared Windows machine (CPU s, 11 interleaved runs): fasta 4.69 / 5.11 → 4.56 / 4.95
(min / median, −3%); pidigits `main_limbs` is within noise (1.58 / 1.72 → 1.64 / 1.78, and
−9% min in an earlier 5-run set). n-body, binary-trees, spectral-norm and the whole `bench/`
micro suite compile to identical IR. See bench/RESULTS.md "Compiler perf round 3".

### 8.2 ~~Locals of an `async` function live in the frame, even in await-free loops~~ (lowering / velt_opt, done)
Affects reverse-complement: its byte loop runs about 2× slower than the same loop in a sync
function, which is most of the 4.66× gap. Also every hot loop in an `async main`.

`$poll` functions are emitted without parameter attributes, and the loop body contains calls
(`velt_rt_realloc`, child polls). So LLVM cannot keep `seq.ptr/len/cap` in registers: every
`push` stores the length to the frame and reloads it. Proposed fix, in two parts:
1. Mark the frame param `noalias nonnull dereferenceable(<frame size>)`. The executor owns the
   frame exclusively during a poll.
2. ~~In `velt_opt`, promote frame slots to SSA temporaries across regions with no suspension
   point.~~ Done: `velt_opt::frame_slots` (the soundness argument is in its module docs). In
   each `<f>$poll`, the scalar frame fields read inside a loop are kept in locals: loaded on
   entry, stored back before every `return` (the suspension points), and stored / reloaded
   around the few statements that touch their bytes as a whole aggregate or through a
   reinterpreted view. Fields whose address reaches a call or a memory copy (embedded child
   states, `Promise.all` result buffers), fields sharing bytes with another field (the state
   layout overlaps locals that are never live together), and functions that pass the frame
   pointer itself anywhere are left alone. Pointers `q = &(*state).f` that are only
   dereferenced are followed, so `seq.push(b)` in `async main` (whose grow path takes
   `&state.seq`) keeps `seq.ptr/len/cap` in registers. On Windows (i9-12900HK, CPU s, best of
   7 interleaved): reverse-complement 1.27 → 1.06, the new `bench/async/hot_loop` 0.41 → 0.13
   (Cranelift release 0.42 → 0.28); see bench/RESULTS.md "Backend round".

   Part 1 (perf round 3): lowering marks poll functions (`vir::Function::is_poll`, which
   replaces `frame_slots`' name / signature heuristic) and gives the frame `nonnull
   dereferenceable(<frame size>)`. `noalias` is **not** unconditional: a poll that hands part of
   its frame to a call (a child's `$poll`, `velt_rt_all`'s result buffer) lets that callee keep
   the pointer and use it in a later poll from code that never receives the frame, which breaks
   LLVM's `noalias` contract (the reason rustc emits no `noalias` for a generator's
   `Pin<&mut Self>`). `velt_opt::frame_slots` adds `noalias` exactly when no frame address
   leaves the function (only dereferences and memory copies), which covers `bench/async/hot_loop`
   after inlining. No measurable change there (0.13–0.16 s, within the 15.6 ms CPU-time tick);
   Cranelift has no counterpart to `noalias`, so it stays as `frame_slots` left it.

### 8.3 ~~Allocation goes through out-of-line runtime wrappers without allocator attributes~~ (codegen / runtime, done)
Affects binary-trees: Velt is 1.27× Rust `Box` + mimalloc (`binary-trees_box`), with the same
code apart from the allocator call. It also affects n-body `main_opt` at 1.85× (169 loads and 80
stores per step).

There are two causes:
1. **The wrapper.** `velt_rt_alloc` / `velt_rt_free` re-validate the `Layout` and take mimalloc's
   aligned path. Proposed fix: for a constant size and an alignment of 16 or less (every `new C()`),
   call `mi_malloc` / `mi_free` directly.
2. **No allocator attributes.** LLVM doesn't know the result is a fresh allocation, so an array
   built by `push` in a loop is a phi of `alloc` / `realloc` results that "may alias". Proposed
   fix: declare the three functions with `allockind`, `allocsize` and an `"alloc-family"`, as
   rustc does for `__rust_alloc`. This is untested.

For n-body, building the same arrays from literals (one allocation) brings the loop down to 33
loads / 17 stores and cuts the time by 13–27%. That is why the std preallocation in 8.6 matters.

Done (both parts, no ABI change):
1. `crates/velt_rt/src/mem.rs` calls mimalloc's C entry points directly: `mi_malloc` /
   `mi_realloc` for alignments up to 8 (every mimalloc block is 8-aligned), the `_aligned`
   variants above, `mi_free` for every free. binary-trees on Windows (CPU s, best of 5
   interleaved): 8.31 → 6.75.
2. `velt_codegen_llvm::runtime::Allocator` declares the three functions like rustc's
   `__rust_alloc` family: `noalias noundef` result, `allocalign` / `allocptr` params,
   `allockind`, `allocsize`, `"alloc-family"="velt_rt_alloc"`, and the memory effects LLVM gives
   libc `malloc` / `realloc` / `free` (`inaccessiblemem`, plus `argmem` for realloc / free).
   Measured: no change on binary-trees (6.75 vs 6.94, noise) or n-body. n-body `main_opt` keeps
   the same loads and stores: its buffer pointer is a loop phi of a phi of `alloc` / `realloc`
   results, and LLVM's BasicAA gives up on nested phis ("only the trivial lcssa and recursive
   phi cases") before it looks at the calls. So 8.6's preallocation (one `velt_rt_alloc`, no
   phi) remains the fix for n-body.

### 8.4 ~~Regex replace and match iterate captures for every match~~ (runtime, fixed)
Affects regex-redux at 1.44×. `velt_rt_regex_replace` and `velt_rt_regex_exec_all`
(`crates/velt_rt/src/regex/`) use `captures_iter` so they can expand `$` patterns. The per-phase
times match Rust using `captures_iter` exactly, while Rust #6 uses `find_iter`. Proposed fix: use
`find_iter` when the replacement has no `$`, and when the pattern has no groups
(`captures_len() == 1`). This is not measured, since it needs a runtime rebuild. Also, `readAll`
copies the input a second time through `from_utf8_lossy(..).into_owned()`; use `String::from_utf8`
with a lossy fallback.

**Fixed** (std/runtime round; measured as instructions and cycles from
`/usr/bin/time -l` against `origin/main`, because the machine was loaded): matching is one JS-style
loop (`crates/velt_rt/src/regex/matches.rs`) that runs `find_at` without capture tracking when the
pattern has no groups or the replacement has no `$`, and reuses one `CaptureLocations` otherwise.
`readAll` decodes in place (one copy, none when reading bytes). regex-redux: −25% instructions,
−15–20% cycles, peak RSS 331 → 240–265 MB, same output. The same loop fixes JS's empty match right
after a match (difftest regex-empty-after-match).

### 8.5 ~~`Map` hit path and line reading~~ (std prelude / runtime, done)
Affects k-nucleotide at 3.07×. About 75% of its time is in `Map.upsert`, which runs 2–2.5×
slower than an Fx hashbrown map once there are 16 or more keys. A hit touches three bounds-checked
arrays (`slots`, `entryKeys`, then `slots` and `entryValues` again), and values are a 16-byte
`V | null`. Proposed changes to `std/prelude/map.vlt`:
- `find` returns the entry position on a hit.
- Store key and value together in one entry array.
- Mark tombstones in the slot word, so values are plain `V`.
- Drop the extra Fibonacci multiply on integer keys.

These come from reading the IR and have not been measured. Line reading costs about 125 ns per
line: `velt_rt_stdin_read_line` locks stdin for each line and copies it twice. `charCodeAt` is
already covered by item 6.

**Done** (std/runtime round), measured: of the four `Map` proposals only the first pays.
- `lookup` returns the entry position on a hit (no second `slots` load): k-nucleotide −15%
  instructions, −8% cycles with the line reading below.
- One `{ key, value }` entry array: **+21% cycles** (same instructions). Probing reads only keys,
  which pack densest in their own array. Not adopted.
- Dropping the Fibonacci multiply: +13% instructions, +27% cycles. FxHash's top bits alone probe
  longer. Not adopted.
- Values stay `V | null`: plain `V` would keep a deleted value (and its `dispose`) alive until
  compaction.

Line reading goes through one process-wide read-ahead buffer (`crates/velt_rt/src/stdin.rs`): a
buffered line is one memchr and one copy, no lock per line, and `await readLine()` completes
without a trip to the blocking pool. On k-nucleotide that alone is −5% cycles. `FileReader` (the
fs_stream handle code) was out of scope this round and still reads through its per-line lock.

### 8.6 ~~Missing std building blocks~~ (std, done except `new Array(n)`)
- **Preallocated arrays:** `new Array<T>(n).fill(v)` and `Array.from({ length: n }, f)`, backed
  by one allocation (see 8.3). Pending golden: `std_array_new_fill`.
- **Bulk `u8[]` operations** under the `Uint8Array` / `Buffer` names: `indexOf(byte, from)`
  (memchr), `set(src, offset)` (memcpy), `copyWithin`, and a zeroed `new Uint8Array(n)`. Rust and
  Go reverse-complement copy whole lines; Velt must push byte by byte. This also explains
  reverse-complement's 422 MB peak RSS against Rust's 125 MB.
- **`BigInt`:** pidigits needs a 190-line limb class and lands at 3.53× Rust+GMP, but only 1.19×
  the same limb code in Rust. Proposed: a runtime-backed `BigInt` (`num-bigint` or `malachite`)
  with in-place operators, plus `Math.umulh` (or `u128`) so Velt libraries can use 64-bit limbs.
- **An arena:** Rust #5 binary-trees uses bumpalo and runs 3.23× faster than per-node allocation.
  The rules allow "a library memory pool", so a `std/arena` would let an optimized Velt variant
  compete.
- **Synchronous binary stdout:** fasta and mandelbrot write through
  `await openWrite("/dev/stdout")`, which waits on the blocking pool for every block. That adds
  0.2–0.8 s of wall time to fasta. `writeBytes` also copies the whole array (mandelbrot peaks at
  72 MB against Rust's 32 MB), and `/dev/stdout` does not exist on Windows. Proposed: a
  synchronous `process.stdout.write(bytes)`, and handing ownership of the buffer to the runtime.

**Done** (std/runtime round):
- `Buffer.alloc(n)` (zeroed, calloc), and `indexOf(byte, from)` (memchr), `lastIndexOf`,
  `set(src, offset)` (memmove), `copyWithin` and `fill` (memset) on `u8[]`
  (std/prelude/bytes.vlt). reverse-complement `main` now reads all of stdin as bytes
  (`readAllBytesSync`, the runtime's buffer handed over), fills a preallocated output and writes
  it once: 3.1× fewer cycles.
- `std/bigint` on `dashu-int`, which ran the spigot 4× faster than `num-bigint` in a Rust
  comparison, with in-place operators. pidigits `main` is now the Node program on it: −39% cycles
  against the old limb class, which stays as `main_limbs.vlt`. Also `Math.umulh`.
- `std/arena` (typed `u32`-index pool, `reset()` keeps the memory): `binary-trees/main_arena.vlt`
  uses 3.7× fewer cycles than `main`.
- `stdout.write(bytes)` (`import { stdout } from "velt:process"`): synchronous, portable, and ordered
  with `console.log`; 64 KiB and up go straight from the caller's array. fasta and mandelbrot use
  it: −3% cycles for fasta, and mandelbrot peak RSS 76 → 68 MB.
- **Not done:** `new Array<T>(n).fill(v)` and `Array.from({ length: n }, f)` need sema (`new` only
  takes classes; `Array.` static calls only builtin namespaces), so they were left to the compiler
  (since implemented). `fill`, `at` and `findLast` exist. Likewise `process.stdout.write` itself: `process` is
  a builtin namespace.

### 8.7 `shared<T>` can't be read (language)
Affects the multi-threaded k-nucleotide (1010 MB against Rust's 134 MB) and regex-redux (514 MB
against 201 MB). Each task gets `seq.clone()`, because `shared<u8[]>` / `shared<string>` can't be
indexed, have `length` read, or be passed where `T` is borrowed. Proposed: let a `shared<T>` that
doesn't hold a `Mutex` be read like a `T` borrow. It is immutable, so this is sound.

### 8.8 ~~Smaller: `i64 / 2` is a signed divide~~ (velt_opt, done)
Accounts for about 10% of spectral-norm `main`. The product has no `nsw` (integers wrap), so LLVM
can't prove it is non-negative, and `/ 2` becomes `sdiv` (a sign fix-up plus a shift). Rust uses
`usize`. Possible fix: a range analysis for counters that start at 0 and only grow. Low priority.

Done: `velt_opt::divisions`. A range analysis alone cannot prove this dividend non-negative:
`(i + j) * (i + j + 1)` can wrap for large `n`. But the product of two values that differ by an
odd constant is always even, also when it wraps, and an exact division by `2^k` is an
arithmetic shift for either sign. So the pass combines:
- block-local value numbering with known low zero bits (`i + j` is computed twice in `A(i, j)`):
  an exact `x / 2^k` becomes `x >> k`, `x % 2^k` becomes 0;
- an interval analysis with branch refinement and widening, tracking only the locals that reach
  a dividend: for a non-negative dividend, `x / 2^k` becomes `x >> k`, `x % 2^k` becomes
  `x & (2^k - 1)`, and other constants divide unsigned.

spectral-norm's inner loops now compute `ashr` (and LLVM unrolls them 2×). On the Windows x86
machine that is within noise for LLVM (1.28 → 1.22 CPU s at N = 5500: the loop is bound by the
`fadd` chain, not the integer part) and 0.45 → 0.41 s for Cranelift release at N = 3000, where
the signed divide is a hardware `idiv`. Re-measure on the M4, where the finding came from.

### 8.9 Algorithm gaps (no compiler fix)
The idiomatic ports use the simpler loop shapes of the Node and Go programs. A scratch Rust program
with exactly the Velt loops runs at parity or slower:

| program | Velt `main` × | same-shape Rust | Rust-shaped Velt × |
|---|---:|---|---|
| mandelbrot | 4.88 | parity (15.0 vs 15.4 s) | `main_opt` (Copy struct of 8 `f64` lanes, vectorized) 1.12 |
| spectral-norm | 1.75 | parity with `>> 1` (8.8) | `main_opt` (two-lane Copy struct) 1.18 |
| fannkuch-redux | 1.44 | parity | `main_mt` on Rust #4's algorithm: 1.06 CPU, 1.10 wall |
| n-body | 1.71 | Rust with `Box<Body>` is slower | `main_opt` 1.85, see 8.3 |
| fasta | 1.95 | integer thresholds bring it to 1.1× | — |

One caveat remains: after the 8.3 fix, n-body is still 1.3× a Rust `Vec` version with the same
optimized IR. It was attributed to Rust's LLVM 22 vectorizing more than Apple clang 21. Re-measured
on x86_64 (RESULTS "Codegen round"): clang 22 and clang 18 compile `main.vlt` and `main_opt.vlt`
to equally fast code (1.5–1.6× Rust #3 without `target-cpu`), so the LLVM version does not explain
the gap there; the loop shape does (see above). Not re-measured on Apple silicon.
