# Handoff: mac-36ea, crew task #123 (#2196 9d stack, #3241 yield order, #3240), 2026-11-20

## PRs and branches (heads as of this handoff)
| PR | branch | head | state |
|---|---|---|---|
| #3249 (#2196 9d-1: stored yieldNow() is a leaf of the executor in Velt; yield hooks in sched_yield.velt) | rt-9d | cd61b8f14 | approved earlier; in train 15 per coordinator |
| #3250 (#2196 9d-2: std records an uncaught callback error's location) | rt-9d2 | 0459668a8 | approved; train 15 |
| #3256 (#3241 PR 1: an awaited yieldNow() resumes after the microtasks queued before it; yield_now_order native-only on wasm, #3307) | yield-order-3241 | ff566fe4f | approved; train 15 |
| #3289 (#3241 PR 2: yields and sleeps take their turns as Node's immediates and timers do) | yield-immediates-3241 | 59ceb967d | open, base yield-order-3241; review R1/R2 fixed; perf not yet within ±3% (below) |
| #3298 (#3240: stored Promise.race over Promise<void> compiles) | race-void-3240 | 57623573d | open, in review |
| wip: two more #3289 cuts + these notes | wip/mac-36ea/3289-cuts | (this branch) | not for review |

- #3249/#3250/#3256 each merge cleanly with main 4e8bc7f1f (after train 14). std-lint passes on main + each.
  None adds a HIR variant (#3256 adds a std hook row `sched_yield_ready`), so nothing for the units codec.
- #3185 landed (train 12d). #3298 has a probe comment: a non-void Promise.race settles one microtask tick
  earlier than Node (the coordinator files it as an issue).

## #3289: what it is (see pr4_body.md here for the full description draft)
Per-task yield list in a yield block (std/lang/hooks/sched_yield_list.velt), turns one at a time
(sched_fire.velt: fireDue, fireYields, dueNow, between), stored yields join lazily (stamp = generation |
number; ready once the generation moved on), root yield is task state YIELDED (sched_task.velt), FIRING
state against the use-after-free (review R2), dueNow for review R1, ROOT_ALONE record for the root's place.
Goldens: lang/yield_now_immediates, yield_now_due_sleep, yield_now_turn_passed, yield_now_root_after_turn
(all from Node; the last three plus yield_now_order are in wasm_goldens' UNSUPPORTED, #3307).

## Status of #3289
- Pushed PR head 59ceb967d: Mac checks all green (7 yield goldens at 1/4 threads, yield/sleep/timer/
  immediate/promise/race/spawn/task_/catch_panic/uncaught goldens, 28 std_scheduler suites, std tests,
  precheck; break checks caught: no-root-record, root-always-ready, no-dueNow, no-lazy-join,
  no-generation; no-firing is a race window no golden reaches).
- On this wip branch, two more cuts (not on the PR):
  - e574402ea: a root yield without a yield block is one store; runEnd reads the flags once; between
    reads the state once. Measured std-only in the VM (numbers below); all 7 yield goldens matched.
  - 094a4a811: schedYield @inline("always") with the non-root and ordered-root paths out of line
    (otherYield, rootYieldInOrder). Mac checks green at wind-down (job13.sh: 7 yield goldens at 1/4
    threads, goldens, 28 std_scheduler suites, std tests, break checks, precheck). NOT measured in the
    VM yet (the VM turn was skipped for the wind-down).
- Perf bar (coordinator, note on #123): BOTH each PR within ±3% of its base AND the stack within ±3% of
  main before the stack (9cb569834). race_yield passes (-14.5%/-13.7% vs main's runtime yieldNow).
  At e574402ea (t1/t4):
  - spawn_many: vs #3256 +1.80/+1.60, vs main +3.98/+3.84  -> still over vs main
  - race_select: vs #3256 +1.46/+1.06, vs main +3.70/+5.23 (t4 noisy) -> over vs main
  - timers_due_reversed: vs #3256 +4.12/+3.46, vs main +2.56/+3.15 -> over vs #3256
  - timers_due_stored: vs #3256 +2.80/+2.28, vs main +1.29/+2.09
  - pingpong_colocated: vs #3256 +0.4/+0.5, vs main +0.6/+3.5 (#3256 alone is +3.0% vs main at t4)
- What's left, in order:
  1. VM A/B of 094a4a811 (or the next cut) vs #3256 and vs main; profiles: ab6-prof.sh b timers_due_reversed;
     ab6-prof.sh m spawn_many race_select.
  2. timers_due_reversed: profile it vs #3256 per line (between() per due sleep is the suspect; forcing
     resume/between inline made it worse earlier).
  3. spawn_many/race_select vs main: remaining cost is the root yield + its resume check (t.main()+E_VT
     compare 3 instr each), and inlining shifts in race_select (schedShare, node.run/release no longer
     inlined where main inlined them).
  4. When all rows pass: squash #3289 (reset --soft to yield-order-3241's head, one commit), push, fill
     pr4_body.md's BENCH_TABLE with the stack table (vs #3256 and vs main-before-9d, t1/t4) and update the
     PR description from pr4_body.md; post r3289reply.md-style reply; ask for re-review.
  5. Retarget #3289 to main once train 15 lands #3249/#3250/#3256 (rebase --onto origin/main ff566fe4f).

## How to rerun the measurements (OrbStack VM velt-prof, shared slot, ask the coordinator first)
- A/B (two release builds, ~70 min): `orb -m velt-prof bash -lc 'setsid nohup bash <repo>/notes/3289/ab4.sh > ~/ab4.log 2>&1 < /dev/null &'`.
  ab4.sh: REPO = the worktree with #3289's std checked out (it copies std/ and tests/golden/lang
  differences vs BASE=ff566fe4f from that worktree); compiler built from ff566fe4f (#3256's top), b = its
  std, n = the worktree std; plus main 9cb569834 (tc-m) with bench/async/race_yield.velt copied in.
  Clean up: ab4-clean.sh, then `orb -m velt-prof -u root fstrim -av`. Paths in the scripts point at
  /Users/andii/dev/velt/branches/yield-order-fix and /Users/andii/dev/velt/branches/notes-9d; adjust.
- Quick std-only re-measure with the compilers still in ~/ab-3289: ab4-n4.sh. Profiles: ab6-prof.sh X rows
  (X = b or m), writes per-line diffs (*-ldiff.txt).
- Mac checks (~35 min, a slot): job13.sh (set REV; builds in rt-9d's target; goldens, suites, std tests,
  break checks on a copy of std via VELT_STD, precheck). Gotcha: perl alarm kills `velt run` but not the
  program it started; job13 now kills leftovers by PID.

## Local-only state
- Worktrees: ~/dev/velt/branches/yield-order-fix (this branch / yield-immediates-3241), ~/dev/velt/branches/rt-9d
  (race-void-3240 for #3298; its target/ is the build dir, removable), ~/dev/velt/branches/rt-symbol (#3185
  landed: removable). Notes copy: ~/dev/velt/branches/notes-9d (all here in notes/3289).
- VM: nothing kept (ab dirs removed after each run).

## Tell yourself
- Measure stack vs main, not just vs the PR's base: four legal pieces can sum to a regression.
- Inlining budget shifts move rows by ±4% on code that wasn't touched (race_small, await_chain): check
  per-function profiles before chasing.
- Wait for jobs by saved PID; pgrep -f matches your own wait loop.
