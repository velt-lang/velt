# Benchmark results

The latest measurement of each suite in `bench/`. Every table names its machine, date and the
compiler commit it measured ("not recorded" where the run predates the repository's history);
the rules for this file are in [README.md](README.md#performance-docs). The Benchmarks Game,
web, HTTP and other suites keep their results in their own folders (see the README).

## Micro suite (`bench/*.velt`)

`bench/run.sh 10` (Windows: `pwsh bench/run.ps1 -Runs 10`): best wall-clock time of 10 runs in
milliseconds, process start included. Every configuration prints the same output as the Rust
version; the harness checks this.

Intel i9-8950HK (6 cores, 12 threads), 30 GB, Linux (kernel 7.2.8); clang 23.1.3 (LLVM backend),
rustc 1.99.0 (`rustc -O`), Node 26.12.0. 2026-10-23, `main` at 909c789a. Another build ran on the
machine throughout (load average 2–8), so the harness ran twice and each cell is the best of both
runs. Ratios above 1 against Node are bold.

| benchmark | Velt cranelift debug | Velt cranelift release | Velt LLVM release | Rust -O | Node | Velt / Rust | Velt / Node |
|---|---:|---:|---:|---:|---:|---:|---:|
| classes | 695 | 237 | 243 | 338 | 750 | 0.72 | 0.32 |
| closures | 1111 | 658 | 341 | 321 | 1253 | 1.06 | 0.27 |
| closures_borrowed | 161 | 80 | 82 | 219 | 77 | 0.37 | **1.06** |
| closures_held | 158 | 46 | 21 | 14 | 83 | 1.50 | 0.25 |
| closures_this | 320 | 129 | 82 | 352 | 113 | 0.23 | 0.73 |
| fib | 54 | 55 | 26 | 30 | 153 | 0.87 | 0.17 |
| floats | 83 | 74 | 67 | 64 | 114 | 1.05 | 0.59 |
| hashmap | 275 | 194 | 136 | 237 | 520 | 0.57 | 0.26 |
| int32 | 4514 | 1508 | 1703 | 1538 | 1524 | 1.11 | **1.12** |
| loops | 2255 | 359 | 147 | 148 | 1344 | 0.99 | 0.11 |
| nbody | 1863 | 612 | 218 | 220 | 2524 | 0.99 | 0.09 |
| shapes | 146 | 121 | 67 | 74 | 553 | 0.91 | 0.12 |
| sort | 217 | 173 | 73 | 74 | 694 | 0.99 | 0.11 |
| strings | 261 | 222 | 117 | 232 | 636 | 0.50 | 0.18 |

The programs (the Rust and Node versions are the same algorithm, written idiomatically):
- **fib**: recursive `fib(35)`.
- **loops**: the longest Collatz chain for starts below 1M, plus a 4000×4000 multiply/modulo loop.
- **floats**: a 600×400 Mandelbrot (200 iterations max) plus 20M-step midpoint integration of
  4/(1+x²).
- **int32**: the 32-bit hash loop of issue #521 on numbers, 2×50M steps as written
  (`(y * k) | 0`, which JS rounds through a double) and 2×50M with `Math.imul(y, k)`. Rust
  computes the same values (`bench/rust/int32.rs`: the rounding by an `i64`→`f64`→`i64` round
  trip, then `wrapping_mul`).
- **nbody**: the Benchmarks Game n-body, 5M steps; `Vec3` / `Body` classes in an array, small
  methods (`add`, `sub`, `scale`, `dot`), `Math.sqrt`.
- **hashmap**: 1M inserts and 1M lookups (half misses) in a `Map<i64, i64>`, then a word count
  of 1M generated words in a `Map<string, i64>` (`upsert` per word). Rust uses std's `HashMap`
  (SipHash) and `entry()`; `bench/rust/hashmap_fx.rs` is the same with an FxHash hasher.
- **sort**: `sort()` of 1M pseudo-random i64 and of 200k generated strings. Rust uses
  `sort_unstable` (Velt's `sort()` is unstable too).
- **classes**: 20 passes of virtual `area()` calls over a `Shape[]` of 1M mixed subclasses
  (plus a non-overridden method), then 20 passes of interface calls over a `Scorer[]` of 1M
  structs. Rust uses `Box<dyn Trait>` for both.
- **closures**: 20 rounds of `map` / `filter` / `reduce` / `forEach` over 1M elements with
  capturing arrows, then 20M calls of a stored (escaping) closure. Rust materializes every
  `map` / `filter` into a `Vec` like JS does, and stores the closure as `Box<dyn Fn>`.
- **closures_borrowed**, **closures_held**, **closures_this**: closures created in a loop and
  only called where they are created (capturing numbers and a string, or `this`); their
  environments live in the frame.
- **strings**: 1M template-literal lines, `join("\n")`, then a `charCodeAt` scan of the 24 MB
  result; then 100k `s += "…"` appends and 100k `` t = `${t}…${i}…` `` appends. Rust uses
  `format!`, `join`, a byte loop, `push_str` and `write!`.
- **shapes**: 1M shapes in a discriminated union (`{ kind: "circle"; r } | ...`), 40 passes of
  `switch (s.kind)` plus a `s.kind === "empty"` test. Rust uses an enum and `match`, Node plain
  objects and `switch`.

How to read it:
- The LLVM release build is at Rust parity, and ahead on `classes`, `hashmap` and `strings`,
  where the Rust versions pay for a `Box` per object, SipHash and `format!`.
- Cranelift release is 0.9–2.8× LLVM release: Cranelift does not vectorize, hoist loop
  invariants or turn constant divisions into multiplies. Cranelift debug (velt_opt off) spends
  its extra time where inlining matters: `loops`, `nbody` and `closures`.
- Slower than Node: `closures_borrowed` (1.02× in an interleaved best of 10): `%` on `number`
  values returned from a function is a `noinline` call, so the constant divisor becomes a 64-bit
  `idiv` ([#1470](https://github.com/velt-lang/velt/issues/1470)). `int32` is within noise of
  Node (0.95–0.96× in two interleaved runs); each half on its own Velt is ahead.
- `int32` as written (`(y * k) | 0`) pays the rounding JS requires: an `i64` product,
  `cvtsi2sd` and `cvttsd2si` on the dependency chain of each multiply. V8 emits the same
  sequence. With `Math.imul` the loop compiles to the instructions Rust emits.

## Typical TypeScript workloads (`bench/typical`)

`bench/typical/run.sh 7`: 19 programs written the way TypeScript code is written (`number`
parameters, classes, closures, `Map<string, number>`, comparator sorts, string building). Each
`.velt` file is also the TypeScript program Node runs (the harness appends the `main();` call),
and `rust/<name>.rs` is an idiomatic Rust port that allocates where JavaScript does (`Box` per
class instance, `Box<dyn Fn>` for stored closures); `vec2.rs` uses a `Copy` struct. All three
print the same output. Same machine, date and commit as the micro suite above (909c789a); best
of two runs of best-of-7, wall ms.

| benchmark | Velt (LLVM release) | Rust -O | Node | Velt / Rust | Velt / Node |
|---|---:|---:|---:|---:|---:|
| consume | 685 | 195 | 722 | 3.51 | 0.95 |
| record | 149 | 106 | 362 | 1.41 | 0.41 |
| strings | 894 | 855 | 5942 | 1.05 | 0.15 |
| vec2 | 51 | 50 | 491 | 1.02 | 0.10 |
| numloop | 140 | 144 | 495 | 0.97 | 0.28 |
| graph | 99 | 103 | 353 | 0.96 | 0.28 |
| fib | 44 | 48 | 190 | 0.92 | 0.23 |
| stats | 59 | 70 | 213 | 0.84 | 0.28 |
| wordcount | 150 | 180 | 813 | 0.83 | 0.18 |
| callbacks | 105 | 129 | 696 | 0.81 | 0.15 |
| grid | 100 | 129 | 577 | 0.78 | 0.17 |
| sortcmp | 159 | 211 | 954 | 0.75 | 0.17 |
| tokenize | 108 | 145 | 261 | 0.74 | 0.41 |
| objects | 168 | 229 | 915 | 0.73 | 0.18 |
| trees | 28 | 43 | 165 | 0.65 | 0.17 |
| dispatch | 49 | 80 | 188 | 0.61 | 0.26 |
| keys | 96 | 189 | 419 | 0.51 | 0.23 |
| chains | 274 | 614 | 1698 | 0.45 | 0.16 |
| errors | 68 | 272 | 220 | 0.25 | 0.31 |

Velt is faster than Node on all 19 and faster than Rust on 15. The open gaps to Rust are
`record` (1.41×) and `consume` (3.5×); the performance survey
[#529](https://github.com/velt-lang/velt/issues/529) tracks them.

## Async (`bench/async`)

`bench/async/run.sh 5` (Windows: `pwsh bench/async/run.ps1 -Runs 5`). Every program is in
`bench/async/`: the Velt source, `node/<name>.js`, and a tokio crate in `rust/` (one binary per
benchmark; the argument `current` selects tokio's current-thread runtime). Velt is built with
`velt build --release` (LLVM) and runs with one worker per core and with `VELT_THREADS=1`. The
harness checks that all five configurations print the same output. The harness builds every
`bench/async/*.velt`; the tables below show five of them.

These tables predate the executor in Velt (`std/lang/hooks/sched.velt`, every native target
now): Velt ran on the runtime's tokio-based executor, and `Promise.all` over an array was the
runtime's join (`crates/velt_rt/src/task/all.rs`). The executor in Velt has not been measured
here yet.

Intel i9-12900HK laptop, Windows 11, clang 22.1.8, Node 22.22.0, rustc 1.93.1, tokio 1.53,
futures 0.3 (Rust uses the system allocator; the Velt runtime links mimalloc). 2026-09, commit not
recorded; other builds were running, so expect ±15–20%. Best of 5, wall-clock ms including
process start:

| benchmark | Velt (all cores) | Velt (1 thread) | Rust tokio multi-thread | Rust tokio current-thread | Node |
|---|---|---|---|---|---|
| await_chain | 55 | 57 | 52 | 52 | 508 |
| await_deep | 379 | 335 | 433 | 502 | 743 |
| fanout_all | 138 | 129 | 145 | 133 | 114 |
| spawn_many | 614 | 697 | 703 | 453 | 1618 |
| timers | 70 | 102 | 64 | 3210 | 229 |

Peak working set (MB, max over the runs):

| benchmark | Velt (all cores) | Velt (1 thread) | Rust tokio multi-thread | Rust tokio current-thread | Node |
|---|---|---|---|---|---|
| await_chain | 6.4 | 5.7 | 4.6 | 4.4 | 57.7 |
| await_deep | 6.4 | 5.7 | 4.7 | 4.5 | 71.7 |
| fanout_all | 7.2 | 6.2 | 4.9 | 4.7 | 71.7 |
| spawn_many | 436.1 | 435.1 | 394.6 | 402.5 | 893.6 |
| timers | 45.9 | 44.9 | 29.1 | 28.6 | 168.0 |

The programs:
- **await_chain**: 10M sequential `await step(acc, i)` of an async function that returns at once.
- **await_deep**: `await deep(20, i)` 500k times; `deep` awaits itself recursively (21 async
  frames per call).
- **fanout_all**: 1000 rounds of `Promise.all` over 1000 calls of a small async function
  (Velt: lazy promises, started by the join; Rust: `futures::future::join_all`, no spawn;
  Node: eager promises).
- **spawn_many**: 1M `spawn(work(i))` (trivial arithmetic plus one `yieldNow()`), then
  `Promise.all` over the handles (Rust: `tokio::spawn` + `yield_now`, handles awaited in order;
  Node: a started async call per task, `yieldNow` as a `setImmediate` promise).
- **timers**: 100k concurrent `sleep(1)` calls joined with `Promise.all` (Rust: `join_all` of
  `tokio::time::sleep`; Node: `setTimeout` promises).

How to read it:
- **await_chain / await_deep**: an awaited `async` call is a direct call into a state machine
  embedded in the caller's state, inlined like a plain call. Node pays a promise and a microtask
  per `await`. Recursion needs one heap frame per level in both Velt and Rust.
- **spawn_many and timers** are at tokio's level. The runtime's join never polled a child once
  tokio's cooperative budget was spent and paid one budget unit per finished child, so a large
  join stayed linear; Rust's idiomatic `join_all` on tokio's current-thread runtime hits the
  quadratic case (3.2 s for `timers`). `bench/async/rust/src/bin/repro_all_budget.rs`
  reproduces both. The executor in Velt's join (`std/lang/hooks/sched_all.velt`) polls only the
  children that woke and spends no budget.
- **Memory**: an idle Velt process is about 2 MB above Rust (mimalloc arenas, one worker thread
  per core) and about 50 MB below Node. 1M live tasks cost about 430 B per task in Velt, 390 B in
  tokio and 840 B in Node.

## Iteration (`bench/iter`)

`bench/iter/run.sh 7` (`run.ps1`): 20 × 30M values summed modulo a prime (a loop-carried
dependency the vectorizer leaves alone), LLVM release, best of interleaved runs. Apple M4, macOS,
shared with other builds (identical programs vary by up to ±10% between rounds); Node runs once.
2026-10-03, commit a8bd90c3f.

| program | Velt (ms) | vs hand_loop | Node (ms) |
|---|---|---|---|
| hand_loop: `while` loop | 1172 | 1.00 | 2649 |
| gen_loop: `for...of` over `range(n)`, a `function*` | 1233 | 1.05 | 77980 |
| iterable_class: iterator class (`next()` called directly) | 1147 | 0.98 | 17341 |
| gen_value: `range(n)` passed as `Iterable<i64>` | 2997 | 2.56 | 74180 |

The async pair: 20 × 3M values, each from an async call that completes at once (`await
step(i)`), on a quieter run of the same machine (sync rows in that run: hand_loop 456 ms,
gen_loop 454):

| program | Velt (ms) | vs async_hand | Node (ms) |
|---|---|---|---|
| async_hand: `while` loop in an async function | 49 | 1.00 | 1876 |
| async_gen: `for await` over `values(n)`, an `async function*` | 118 | 2.41 | 7043 |

Channels as async iterables, `for await (const v of ch)` against an `await ch.receive()` loop
(interleaved, best of 21, `VELT_THREADS=1`):

| program | `receive()` loop (ms) | `for await` (ms) |
|---|---|---|
| `bench/async/channel_pipeline.velt` (4 producers, 1M values, capacity 1024) | 44.6 | 46.8 |
| drain: 10 × 1M values queued, then received in one task | 432.6 | 428.6 |

- gen_loop's hot loop is the hand loop's instructions: the generator's state lives in the loop's
  frame, its resume function is inlined and its dispatch is jump-threaded away; nothing is
  allocated. Where the hand loop vectorizes, the generator loop stays scalar: the `yield` is an
  exit from the producer's loop.
- gen_value allocates the generator object once per loop; each value costs an interface call of
  `next()`, a table call of the resume function and an `IteratorResult` value.
- An async generator's state is part of the caller's state and is polled by a direct call per
  value; nothing is allocated. The difference to the hand loop (about 1.2 ns per value) is that
  state living in memory rather than registers. A channel's `[Symbol.asyncIterator]` is such an
  async generator method.

## JSON (`bench/json`)

`bench/json/run.sh 5`: each program in `bench/json/` (Velt LLVM release), a Rust version with
serde (derive) and serde_json (`preserve_order`, so objects keep insertion order like JavaScript
and Velt), and a Node version; the harness checks that all print the same output. Same machine,
date and commit as the micro suite (909c789a), best of two runs.

| benchmark | Velt (LLVM release) | Rust serde_json | Node | Velt / Rust | Velt / Node |
|---|---:|---:|---:|---:|---:|
| edit | 23 | 1398 | 83 | 0.02 | 0.28 |
| navigate | 472 | 737 | 289 | 0.64 | **1.63** |
| parse_typed | 319 | 511 | 657 | 0.62 | 0.49 |
| parse_union | 370 | 307 | 582 | 1.21 | 0.64 |
| parse_value | 2748 | 1565 | 2170 | 1.76 | **1.27** |
| stringify | 295 | 197 | 355 | 1.50 | 0.83 |

`parse_value` takes 0.9 s or 2.7 s from one run to the next on this machine, for Velt and Node
alike; interleaved runs, best of 7: Velt 2748 ms, Rust 4268 ms, Node 2238 ms.

- **parse_typed**: `JSON.parse<Item[]>` of 100k objects (8 MB), 8 times (Node: `JSON.parse`).
- **parse_union**: `JSON.parse<Shape[]>` of 200k objects of a two-member union, the
  discriminant first in one member and last in the other, 8 times (Rust: an internally tagged
  enum).
- **stringify**: `JSON.stringify` of 100k structs, 8 times.
- **parse_value**: `JSON.parseValue` of the parse_typed document, 8 times.
- **navigate**: `at(i)` and `get(key)` over the parsed document, 20 passes.
- **edit**: `set` 10k keys into an object, then `delete` them all from the front, 4 times
  (Rust: `shift_remove`, which keeps the order and moves the later members, O(n) per delete).
  An edit turns the parsed tape into std's tree (`std/encoding/json/tree_object.velt`):
  deleting is O(1) amortized and `at(i)` after deletes O(log n) (a Fenwick tree of live slots).

`navigate` and `parse_value` are slower than Node; survey
[#529](https://github.com/velt-lang/velt/issues/529) tracks them.

### JSON corpora (`bench/json/corpora`)

`MEASURE=instructions bench/json/corpora/run.sh 3 50`: the three files JSON libraries report on
(`fetch.sh`: canada.json, 2.25 MB of GeoJSON floats; citm_catalog.json, 1.73 MB of wide objects
and integers; twitter.json, 0.63 MB of text with non-ASCII), each parsed into a dynamic value 50
times (`value`) and parsed once then written back 50 times (`write`), by Velt (LLVM release),
serde_json, sonic-rs, simd-json and Node.

- **What is measured**: the whole process, start-up and the file read included; the fewest
  instructions retired (`/usr/bin/time -l`) of 3 runs, and in brackets per input byte (× 50
  rounds). An empty run retires about 1.4 M instructions for Velt and 3.8 M for `node -e 0`.
  Instructions understate what SIMD parsers gain per instruction and ignore cache misses, so read
  them as work done, not speed.
- **What each builds**: Velt's `JSON.parseValue` builds a flat tape; serde_json a `Value` tree
  with `preserve_order` and `float_roundtrip` (exact floats, as Velt and Node parse them);
  sonic-rs its own DOM; simd-json an `OwnedValue` (unordered objects; it copies the input every
  round because it parses in place); Node `JSON.parse` / `JSON.stringify`.
- **Checks**: for `value`, every program prints the same top-level member count. For `write`,
  Velt's last text must equal Node's byte for byte, and the Rust rows must parse back to the same
  value as Node's.

Apple M4, macOS 27.0.1 (arm64), rustc 1.99.0, serde_json 1.0.154, sonic-rs 0.5.10, simd-json
0.19.0, Node 24.11.1. 2026-10-28, `main` at 0d0efe524. Billions of instructions (per input byte);
fewer is better:

| corpus | op | Velt | serde_json | sonic-rs | simd-json | Node |
|---|---|---:|---:|---:|---:|---:|
| canada | value | 7.40 G (66/B) | 5.39 G (48/B) | 1.85 G (16/B) | 4.50 G (40/B) | 5.98 G (53/B) |
| canada | write | 5.54 G (49/B) | 1.67 G (15/B) | 1.98 G (18/B) | 2.48 G (22/B) | 6.69 G (59/B) |
| citm_catalog | value | 2.57 G (30/B) | 2.68 G (31/B) | 0.61 G (7/B) | 1.90 G (22/B) | 2.32 G (27/B) |
| citm_catalog | write | 0.61 G (7/B) | 0.46 G (5/B) | 0.57 G (7/B) | 0.49 G (6/B) | 1.36 G (16/B) |
| twitter | value | 1.39 G (44/B) | 1.06 G (34/B) | 0.26 G (8/B) | 0.78 G (25/B) | 1.03 G (33/B) |
| twitter | write | 0.36 G (11/B) | 0.29 G (9/B) | 0.26 G (8/B) | 0.27 G (9/B) | 0.88 G (28/B) |

Intel i9-8950HK, Linux, load average 26–43, Node 26.12, sonic-rs 0.3, simd-json 0.14; 2026-10-28,
the same Velt commit. `nice bench/json/corpora/run.sh 2 50`, best wall time in ms (MB/s):

| corpus | op | Velt | serde_json | sonic-rs | simd-json | Node |
|---|---|---:|---:|---:|---:|---:|
| canada | value | 3149 (36 MB/s) | 2722 (41 MB/s) | 795 (142 MB/s) | 2228 (51 MB/s) | 2469 (46 MB/s) |
| canada | write | 2133 (53 MB/s) | 731 (154 MB/s) | 1035 (109 MB/s) | 878 (128 MB/s) | 1155 (97 MB/s) |
| citm_catalog | value | 1034 (84 MB/s) | 1026 (84 MB/s) | 374 (231 MB/s) | 1014 (85 MB/s) | 881 (98 MB/s) |
| citm_catalog | write | 261 (331 MB/s) | 210 (411 MB/s) | 201 (430 MB/s) | 210 (411 MB/s) | 543 (159 MB/s) |
| twitter | value | 627 (50 MB/s) | 492 (64 MB/s) | 152 (207 MB/s) | 450 (70 MB/s) | 487 (65 MB/s) |
| twitter | write | 137 (230 MB/s) | 120 (264 MB/s) | 93 (340 MB/s) | 148 (213 MB/s) | 327 (97 MB/s) |

Velt's dynamic parse retires 1.2–1.4× Node's instructions on canada and twitter and about the
same as serde_json on citm_catalog; sonic-rs does 4–5.3× less work. Velt writes citm_catalog and
twitter within 1.35× of serde_json and with 2.2–2.4× fewer instructions than Node; canada
(floats) takes 3.3× serde_json's instructions, so number formatting is the gap there. Typed parse
against serde derive structs and a Bun column are not covered
([#1657](https://github.com/velt-lang/velt/issues/1657)).

## Sorting with a comparator

`sort(cmp)` is a stable sort shaped like Rust's driftsort (`std/lang/sort/stable.velt`).
Instructions and branch mispredictions of the sort alone (a run with the sort minus a run with
only the setup), valgrind cachegrind, Linux (WSL2), clang 18, Rust `sort_by` (`rustc -O`).
2026-10-06, commit 165e66282.

| workload | Velt | Rust `sort_by` | Velt / Rust |
|---|---:|---:|---:|
| 1M `number`s, `(a, b) => a - b` | 619M, 1.5M mispred | 328M, 1.2M | 1.89 |
| 1M `i64`, `(a, b) => a - b` | 507M, 1.5M | 259M, 1.1M | 1.96 |
| 300k objects by `age`, then `name` | 398M, 1.1M | 252M, 3.0M | 1.58 |
| 100k objects by a float field | 146M, 0.1M | 82M, 0.0M | 1.78 |
| 1M `number`s, `sort()` (pdqsort) | 427M, 1.5M | 264M, 0.7M (`sort_unstable`) | 1.62 |

- The quicksort's partitions are branch-free (each element is written to both ends of the buffer
  and one cursor moves); merges of runs already in order are skipped, so sorted and reversed
  inputs are O(n).
- velt_opt inlines the comparator through `cmp: F | null` and through every closure argument of
  a recursive helper; without that, objects sorted by name cost 447M instructions.
- The sort works on a buffer of its own while the comparator runs, because a comparator can reach
  the array through an alias. A short array (at most 20 elements) of elements that need no drop
  is insertion sorted in place when the comparator captures nothing.

## Callbacks over an aliased array

Array callback methods pass the callback a share of the element when the array may be reached
through another reference, and re-check the length after each callback; without such a
reference the plain loop is all that is compiled. Each program has 2000 elements and runs 1000
rounds of `xs.forEach((x, i) => …)` plus `xs.map((x, i) => …)`: 4M callback calls. "Alias" means
`const ys = xs` exists; "field" is a `string[]` field of an object that two variables hold.
Instructions (valgrind cachegrind, Linux in WSL2, programs built with LLVM `--release`).
2026-10-07, commit 203e3cf4b.

| elements | no alias | alias | per call |
|---|---:|---:|---:|
| `string[]` | 89.63M | 315.65M | +56.5 |
| `string[]` in a field of a shared object | 89.63M | 305.67M | +54.0 |
| `number[]` | 36.85M | 56.97M | +5.0 |
| `[string, number][]` | 89.64M | 147.77M | +14.5 |
| objects | 89.73M | 147.78M | +14.5 |

Over an aliased `string[]` each call shares the string and drops the share afterwards (two count
updates, plus the fallback when the count check fails); that keeps the argument valid when the
callback pushes or pops through the alias. Pinning the buffer once per method call instead is
[#592](https://github.com/velt-lang/velt/issues/592). Without an alias the code is the same as a
plain loop.

## TSX fortunes (`bench/jsx`)

The TechEmpower fortunes page rendered by hand-written template literals, by precompiled TSX and
by generic TSX: [jsx/README.md](jsx/README.md) has the method and the latest table
(`bench/jsx/run.sh`, with `BASE=<ref>` for an A/B of std/jsx).

## Byte views: decoding b-tree page cells (`bench/typed_array/page_cells`)

SQLite-like b-tree pages (64 pages of 4 KiB, 26,181 cells) in a `Uint8Array`, decoded in plain
TypeScript: per cell a 2-byte big-endian cell pointer, a 4-byte big-endian child page number and
a varint key. Four Velt programs read the same bytes: a `DataView` per page, a `subarray` per
page with shifts, the whole `Uint8Array` with shifts, and `u8[]` with integer types; Rust reads
`&[u8]` with `u16::from_be_bytes` / `u32::from_be_bytes` (bounds-checked). User-space
instructions per cell (`/usr/bin/time -l`, the difference between 110 and 10 rounds), LLVM
release, Apple silicon, macOS arm64. 2026-10-29, commit 2f25b17ee.

| program | instructions per cell |
|---|---:|
| `DataView` | 70.1 |
| `subarray` per page | 70.4 |
| whole `Uint8Array` | 68.2 |
| `u8[]`, integer types | 60.5 |
| Rust `&[u8]` | 40.8 |

A `DataView` big-endian load checks its range once and reads its bytes as one load and a byte
swap; a byte's `xs[i]` tests that it is inside the view and inside the buffer in one integer
test. The remaining gap to Rust is that the key and the sum are `number`s (doubles), as in JS,
where Rust and the `u8[]` program use `u64`.

## Positioned file I/O (`bench/fs/pread`)

`velt/fs`'s `readSync` / `writeSync` / `fsyncSync` are Velt code calling `pread` / `pwrite` /
`fcntl` directly (C library declarations, `std/fs/fd/posix.velt`), against Rust
(`FileExt::read_at`, `sync_all`) and Node (`fs.readSync` with a position). Workloads: 200,000 random 4 KiB reads of a cached 64 MiB
file, 8 sequential passes in 64 KiB reads, 200 100-byte writes each followed by an fsync.
User-space instructions (cachegrind, Linux arm64 VM; the kernel's share of a read is the same for
all, and it is most of the time). 2026-10-28, commit 16ee80604.

| program | instructions |
|---|---:|
| Velt, the whole benchmark through `velt/fs` | 12.85 M |
| Rust, the whole benchmark | 8.18 M |
| Velt, the random reads through a bare `pread` declaration (a one-off program) | 8.03 M |
| Velt, an empty program (runtime start-up) | 0.40 M |

A `pread` from Velt costs what it costs from Rust (about 38 instructions per random read with the
loop); Node's argument checks add about 22 instructions per call, under 1% of a cached 4 KiB
read. No wall-clock table yet: `bench/fs/pread/run.sh 10` on an idle machine gives one.

## Page-header reads from a mapped file (`bench/fs/mapped_header.velt`)

2,000,000 big-endian 32-bit reads at computed offsets of a 64 KiB `MappedFile`, three ways.
User-space instructions (cachegrind, Linux x86-64, release, one thread; a debug-built compiler
with a release runtime), less the 23.49 M the program costs with no reads. 2026-11-20.

| program | instructions | per read |
|---|---:|---:|
| `m.getUint32(at)`: DataView's getter on the mapping | 98.00 M | 49 |
| four `bytes[i]` reads and shifts | 198.01 M | 99 |
| `m.u32s(at, 1)[0]`: a typed array per read (aligned, the machine's byte order) | 237.41 M | 119 |

The getter is one range check and one load; the byte reads check four times, and the array
is allocated, counted and freed for each read. The loop's own arithmetic (a remainder on a
`number`) is in all three.

## Pinned and counted arrays from a mapped file (`bench/fs/mapped_counted.velt`)

2,000,000 reads of one 32-bit value at computed offsets of a 64 KiB `MappedFile`, each through an
array of its own. User-space instructions (cachegrind, Linux x86-64, release, one thread; a
debug-built compiler with a release runtime), less the 19.88 M the program costs with no reads;
wall time the best of 15 interleaved runs, less the same. 2026-11-22.

| program | instructions | per read | time |
|---|---:|---:|---:|
| `m.u32s(at, 1)[0]`: the typed method's array, over the pinned mapping | 241.39 M | 121 | 20.5 ms |
| `m.countedU32s(at, 1)[0]`: a counted array, which holds the mapping | 289.39 M | 145 | 31.8 ms |

A counted array takes one of the holds its `MappedFile` reserved (no atomic) and gives it back
when it is dropped (one atomic decrement, through the owner block's function): 24 instructions
and about half again the time of a pinned array, on a loop that makes one array per value. The
typed methods cost what they did before the counted ones existed (`mapped_header.velt` rows
unchanged).

## Records copied out of a mapped file (`bench/fs/mapped_copy.velt`)

2,000,000 copies of a 64-byte record from computed offsets of a 64 KiB `MappedFile` into a buffer
that is used again. User-space instructions (cachegrind, Linux x86-64, release, one thread; a
debug-built compiler with a release runtime), less the 19.89 M the program costs with no copies.
2026-11-23.

| program | instructions | per copy |
|---|---:|---:|
| `m.bytes.copy(record, 0, at, at + 64)`: `u8[]`'s copy on the mapping's bytes | 196.01 M | 98 |
| `m.copy(record, 0, at, at + 64)`: MappedFile's, into a `u8[]` | 352.01 M | 176 |
| `m.copy(words, 0, at, at + 64)`: the same into a `u32[]` of 16 | 539.42 M | 270 |
| 64 `record[i] = m.bytes[at + i]` | 4,290.01 M | 2,145 |

`m.copy`'s target is a union (`T[] | ArrayBufferView`), which the caller retains and drops around
each call (about 79 instructions), and its arguments are checked twice: by `copy`, then inside
`u8[].copy`. A number array of another type is viewed as bytes first (`addressArray`, which
allocates). Each copy is one `memcpy`; the loop checks every index.

## `f16x8` arithmetic (`bench/simd/half_arith.sh`)

Instructions retired per element, 2^20 elements, 100 passes (each run with 0 and with 100 passes
and subtracted), fewest of 5. Apple M4, 2026-11-09, commit 47336b88f. Rust nightly is
nightly-2026-09-30 (stable Rust 1.99 has no f16 NEON intrinsics); the stable rows are Rust 1.99
with `half` 2.7.1. C is Apple clang 21, `-O2 -mcpu=apple-m1 -ffp-contract=off`. `axpy` is
`y = a * x + y`, `act` is `z = max(x * s + b, 0)`, and `dot` has four `f16` accumulators.

With FP16, Velt's default on Apple arm64:

| kernel | Velt | Rust nightly | C | Velt / Rust |
|---|---:|---:|---:|---:|
| axpy | 0.88 | 0.88 | 0.88 | 1.00 |
| act | 1.00 | 1.00 | 1.00 | 1.00 |
| dot | 0.34 | 0.34 | 0.34 | 1.00 |

Without FP16 (`--cpu cortex-a53`, run on the same M4), against code that widens to `f32`:

| kernel | Velt `mulAdd` | Velt `mulAddFast` | Rust + `half` | C | `mulAddFast` / Rust |
|---|---:|---:|---:|---:|---:|
| axpy | 9.25 | 1.75 | 1.87 | 1.75 | 0.94 |
| act | 9.13 | 2.38 | 2.00 | 1.88 | 1.19 |
| dot | 9.75 | 1.47 | 1.62 | 1.47 | 0.91 |

The checksums agree within each comparison: the first table and Velt's `mulAdd` without FP16 give
the same bits, and `mulAddFast`, Rust with `half` and C widened give the same bits (an FMA in
`f32` rounds twice, so the two sets differ).

- `mulAdd` without FP16 costs about five times `mulAddFast`, the price of one rounding (`f64` and
  a round-to-odd sequence). Without FP16, take `mulAddFast` unless the last bit must match every
  CPU.
- `act` is 1.19× Rust because `mulAddFast` narrows and `max` widens again, where the Rust and C
  kernels stay in `f32` between the two.
- x86 (including AVX512-FP16) has not been measured.

## Compile time

### Front end (`bench/compile/run.sh`)

`bench/compile/run.sh 10` (Windows: `pwsh bench/compile/run.ps1 -Runs 10`) times the stages of
`velt build -v --release --emit vir`, best of N runs per stage, in milliseconds. `http_hello` is
`examples/http_hello.velt`, `all_std` imports every std module, `units_1000` / `chain_1000` are
1000 generated units (`bench/compile/unit.tmpl`: a class and an overriding subclass, a struct
implementing a shared interface, functions modifying an array param; call chains of 8, or one
chain through the whole program), `long_main_<n>` thousands of classes used from one `main`.

Intel i9-12900HK, Windows 11, release `velt`; 2026-10-01, commit not recorded.

| program | lines | parse | sema | lower | verify | optimize |
|---|---:|---:|---:|---:|---:|---:|
| http_hello | | 8.5 | 4.9 | | | |
| all_std | 19 | 13.1 | 5.9 | 0.6 | 0.1 | 9.6 |
| units_1000 | 62132 | 48.1 | 154.4 | 83.6 | 30.3 | 1646.4 |
| chain_1000 | 62008 | 47.3 | 149.5 | 76.4 | 27.7 | 1445.6 |
| long_main_4000 | 84008 | | | 101.2 | 41.9 | 683.3 |

Sema scales linearly: `cargo test -p velt_sema --release --test scaling -- --ignored
--nocapture` checks 100 / 1000 / 5000 units. `velt build --timings` prints each stage, each
optimizer pass and the LLVM backend's IR printing and clang time.

### `velt check` and debug links

The second table of `bench/compile/run.sh 5 1000 <velt>` (Windows: `run.ps1 -Runs 5 -Units 1000
-Velt <velt> -LinkOnly`), best of 5, ms, with `VELT_LINKER=system` and `VELT_LINKER=bundled`
(the bundled lld). The "check" columns do not link.

Apple M4 (10 cores), macOS 27.0.1, the installed toolchain; system linker `cc` → Apple `ld`.
2026-10-09, commit f702a81bc.

| program | linker | `velt check`: parse + sema | `velt check`: whole command | debug link: shared runtime | debug link: static runtime | rebuild, nothing changed: link |
|---|---|---|---|---|---|---|
| http_hello | system | 30.1 | 39.3 | 38.8 | 85.5 | 0.2 |
| http_hello | **bundled** | 25.6 | 32.7 | **7.1** | **53.1** | 0.2 |
| all_std | system | 32.3 | 42.4 | 38.7 | 83.6 | 0.1 |
| all_std | **bundled** | 29.2 | 38.1 | **6.4** | **42.2** | 0.1 |
| units_1000 | system | 171.2 | 188.9 | 58.9 | 91.8 | 3.1 |
| units_1000 | **bundled** | 194.2 | 218.3 | **47.5** | **72.7** | 2.9 |

GitHub `windows-2025` runner (AMD EPYC 9V74, 4 vCPUs), Windows Server 2025, the release
toolchain archive; system linker `link.exe` (MSVC 14.51). Same date and commit.

| program | linker | `velt check`: parse + sema | `velt check`: whole command | debug link: shared runtime | debug link: static runtime | rebuild, nothing changed: link |
|---|---|---|---|---|---|---|
| http_hello | system | 72.6 | 94.3 | 55.6 | 230.8 | 0.3 |
| http_hello | **bundled** | 63.6 | 80.0 | **38.0** | **169.3** | 0.7 |
| all_std | system | 72.0 | 89.4 | 42.4 | 229.2 | 0.2 |
| all_std | **bundled** | 69.4 | 86.0 | **37.1** | **165.8** | 0.6 |
| units_1000 | system | 452.8 | 501.6 | 76.9 | 261.1 | 2.3 |
| units_1000 | **bundled** | 438.2 | 488.6 | **71.5** | **198.0** | 2.9 |

Cloud VM, x86_64 Linux (Ubuntu 24.04), 4 × Intel Xeon @ 2.10 GHz, GNU ld 2.42, LLD 18;
2026-10-01, commit not recorded:

| program | `velt check`: parse + sema | `velt check`: whole command | debug link: shared runtime | debug link: static runtime (lld) | rebuild, nothing changed: link |
|---|---|---|---|---|---|
| http_hello | 11.3 | 16.0 | 29.2 | 92.1 | 0.1 |
| all_std | 14.0 | 18.9 | 29.6 | 97.9 | 0.1 |
| units_1000 | 191.4 | 216.8 | 102.2 | 138.5 | 25.2 |

Debug builds link the shared runtime: the link time is flat in program size, and a debug
executable is tens of KB. On a Windows desktop with real-time protection, Defender scans
`lld.exe` (56 MB) on its first start after a fresh copy; link with `VELT_LINKER=system` when
Build Tools are installed and that is faster.

### Cranelift memory on huge functions

Debug build (`velt build`, Cranelift), peak RSS of the whole command and the codegen stage.
Cloud VM, x86_64 Linux, 4 × Xeon @ 2.10 GHz; 2026-10-02, commit 1d536cfce.

| program | peak RSS | codegen |
|---|---:|---:|
| long_main_1000 | 140 MB | 1.3 s |
| long_main_2000 | 218 MB | 2.8 s |
| long_main_4000 | 367 MB | 6.1 s |
| long_main_8000 | 645 MB | 11.3 s |
| long_main_16000 | 1208 MB | 26.5 s |

`bench/compile/stress.sh` (`stress.ps1`) builds long_main_16000 and fails above 2 GB; the nightly
workflow runs it on Linux and Windows.

### Parsing

`cargo test --release -p velt_syntax --test bench -- --ignored --nocapture` (parse only, the AST
drop not counted). Windows 11, x86_64, 20 logical cores, busy machine; best of 15 (median in
brackets), ms. 2026-10-02, commit 6c6aec932.

| input | ms |
|---|---|
| no JSX, 100k lines | 275.1 (280.8) |
| std and examples (119 files) | 24.3 (25.9) |
| JSX and generics, 50k lines | 60.7 (64.9) |
| JSX goldens (25 files) | 3.63 (3.79) |

### `velt dev` debug info

Each JIT version registers an in-memory ELF image with DWARF line tables for debuggers (GDB JIT
interface); `VELT_DEV_DEBUG_INFO=0` turns it off. Release `velt dev --host --timings`, with and
without the image, 15 interleaved runs each, best (median in brackets), ms; peak RSS from
`/usr/bin/time -l`. log-pipeline is `examples/apps/log-pipeline` running `src/main.velt -- gen
x.log --lines 10`. Apple M4, macOS 26, shared with other builds; 2026-10-02, commit 42bac3abb.

| program | functions (image) | `debug info` step | `jit` stage, on / off | time to first run, on / off | peak RSS, on / off |
|---|---|---|---|---|---|
| log-pipeline | 306 (81 KB) | 0.6 (0.7) | 48.5 / 46.6 | 74 (95) / 78 (95) | 28.2 / 27.7 MB |
| units_1000 | 22,385 (2.9 MB) | 15.7 (24.7) | 855 / 865 | 1241 (1727) / 1134 (2223) | 224.6 / 205.6 MB |

The image stays under the limits for the startup path (5% or 50 ms of startup, 10% of peak RSS),
so it is built by default.

### Large programs (`bench/compile/scale`)

Linux x86-64 builder (12 cores, shared: load 20–48, so stage times are wall clock and inflated;
compare the CPU column). Each run has a fresh empty `VELT_HOME` and `VELT_BUILD_CACHE=off
VELT_BUILD_STAMP=off`. CPU = user seconds of `velt` and its children. 2026-11-03.

sigx's small-page scenario (a UI renderer whose `renderNode` is a 350-line generator), `main` at
915753a2f:

| | CPU s | peak RSS | parse | sema | generate | lower | verify | optimize | codegen | link |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `velt check` | 4.9 | 0.75 GB | 0.31 | 3.75 | 0.74 | | | | | |
| debug build | 613.5 | 1.8 GB | 0.21 | 4.17 | 0.89 | 644.2 | 1.67 | 6.96 | 46.0 | 1.88 |
| release build | 2168.4 | 4.9 GB | 0.31 | 3.58 | 0.92 | 689.4 | 4.32 | 238.8 | 1170.2 | 1.19 |
| debug, `vir: lowering a large function` | 333.3 | 1.8 GB | | | | 520.9 (load 40) | | | | |

Generated programs, `bench/compile/scale/run.sh <lines>`, commit 5e69e7392; Go 1.25.3 builds the
same program, std warmed in a fresh `GOCACHE`:

| lines | `velt check` | debug build | release build | `go build` |
|---|---:|---:|---:|---:|
| 10,116 | 0.6 s | 3.6 s | 53.4 s | |
| 101,577 | 2.7 s | 38.4 s | 2,324 s | 61.6 s (26.5 s wall) |

- Lowering dominates the small-page debug build. It runs the whole program several times (the
  counted-types fixpoint, `lower/boxing/`), and one function dominates each pass: the state
  machine of the generator, at 187,519 blocks in the first pass. `VELT_LOWER_PROFILE=1` names
  the slowest functions per pass.
- Release codegen is dominated by one clang process: at 10k lines the IR is 635,891 lines (63
  per source line) and `main` alone is 32,821, because the inliner merges the units' entries
  into it. At 100k lines the debug build costs about what `go build` does (CPU); the release
  build is the outlier.

## Lessons from optimization work

Results of past experiments that still hold, so they are not repeated:

- **Sorting.** Branchless Lomuto partitioning must compare *before* the swap; comparing the
  element after swapping it into place makes every iteration wait on the previous store (2.5×
  slower). Cranelift keeps `c ? 1 : 0` as a branch: write `bool as usize`. BlockQuicksort
  (offset buffers) was slower than Lomuto, because each offset store forces the array header to
  be reloaded.
- **`Map`.** Robin Hood probing at 3/4 load and plain linear probing at 5/8 run the benchmark
  equally fast; Robin Hood uses half the index memory and is faster on misses at equal load. One
  `{ key, value }` entry array costs +21% cycles (probing reads only keys, which pack densest in
  their own array); dropping the Fibonacci multiply on integer keys costs +27% (FxHash's top bits
  alone probe longer). The remaining gap to hashbrown on misses and deletes is memory: hashbrown
  probes a 1-byte control array where Velt probes 8-byte slot words.
- **Allocator attributes.** Declaring `velt_rt_alloc` / `velt_rt_realloc` / `velt_rt_free` like
  rustc's `__rust_alloc` family measured no gain: LLVM's BasicAA gives up on nested phis of
  allocation results. Preallocating (one allocation, no phi) is what removes the loads and stores.
- **Async frames.** A poll function's frame is `noalias` only when no frame address leaves the
  function: a callee that keeps part of the frame and uses it in a later poll would break LLVM's
  `noalias` contract (the reason rustc emits no `noalias` for a generator's `Pin<&mut Self>`).
- **Release optimization level.** `-O3` stays the default (`VELT_LLVM_OPT` selects another):
  `-O2` saves about 12% of clang time and runs 12–33% slower on nbody, hashmap and sort. clang 22
  and clang 18 compile the same IR to equally fast code on x86_64.
- **Codegen units.** Placing units by the call graph (strongly connected components, a component
  of up to 2 000 statements joins its first referrer's group) keeps hot loops and recursive drop
  glue in one unit: split into 4, every benchmark composite runs within ±3% of one unit. Cut by
  program order, binary-trees' drop glue crossed units at every node (+13–15%). In 8 units fib
  and binary-trees ran 5–8% slower from code alignment alone, so the default stops at 4 until
  each unit would pass 200,000 statements (`crates/velt_codegen_llvm/src/units/mod.rs`).
- **Measuring.** Single wall-clock runs on a shared machine vary by ±10–30%. Compare by
  instructions retired, or interleave A/B runs and take the best of N; when IR is byte-identical,
  a timing difference is the machine.
