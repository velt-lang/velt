## Exception request: inline_all_yields above ±3%

Round 1's fixes and the cost cuts below are on `root-turns-3687-r1-wip` at ec4258839. They'll be pushed here, squashed and rebased onto main, once #3706 lands.

Every other row is within ±3% of #3706:
- On x86-64 at 18cc57d98 (the table above), root_yields_after_all is −0.01%, the root yields without a block −0.02%, race_yield +2.11%, and the rest about 0.
- On arm64 at ec4258839 (median of 5): root_yields_after_all +1.29%, root_yields +0.23%, race_yield +1.30%, fanout_all −0.03%, spawn_many +0.09%.

The x86 table for ec4258839 follows, from the coordinator's run.

One kind of row stays over: inline `Promise.all`s awaited in a task's root whose children yield.

**Instructions per child yield** (`VELT_THREADS=1`, release programs; the coordinator's program: 50,000 rounds of `await Promise.all([yields(2), yields(2)])`, 200,000 child yields):

| | #3706 | 67cd80f87 (as reviewed) | 18cc57d98 (round 1) | ec4258839 (round 1 + cuts) |
|---|---:|---:|---:|---:|
| x86-64 (cachegrind) | 146.7 | 395.1 (+248.4) | 409.1 (+262.4) | follows |
| arm64 (instructions retired, median of 5) | 227.9 | | 504.8 (+276.9) | 391.0 (**+163.1**, +71.6%) |

bench/async/inline_all_yields (50,000 alls of two children that yield once, in nightly.sh's async list) is +62.8% on arm64 at ec4258839, against +97.5% at 18cc57d98.

**Why it costs more.** #3706 gave both children's yields one shared turn, which is the order this PR fixes. In Node each `yieldNow()` is an immediate of its own: the first child's step, and the microtasks and wakes it causes, run before the second child's step. The second child's yield therefore needs a turn of its own. That means:
- a record on the task's yield list;
- the task's poll taking the list's path, not the straight one (`runOther`, `fireYields`, `runEnd`);
- `take`, `fire` and `join`.

`std/promise_combinators_order_root` shows the order this buys. Each of its sections matches Node 22 here and not on #3706's std:
- a microtask queued at a child's turn;
- a child that queues one and yields again;
- the first child in the array yielding second;
- a started promise's yield between two children's;
- children detached by a throw at once.

**What I cut** (arm64, the bench row against 18cc57d98):
1. **Consecutive turns of the root go on in one poll when nothing could run between them** (`sched_yield_turn.velt` `nextTurn`). A held root record whose turn is the next one goes on in the current turn's poll of the root. Conditions: this turn's waiter went on, and no wake of the root, microtask, rejection to report or woken promise is pending. Output is unchanged on the goldens and the probes. Four break checks make the root golden or the token test fail. **−9.7%.**
2. **Each yield block keeps up to 8 records for reuse** (task thread only): **a further −7.0%.**
3. **No record for the root's placed turn while only the root's records are listed.** The next poll runs that turn first, before the records stamped after it. One break makes the golden and the token test fail. **A further −2.7%.**
4. **`sharedRootYield` is inlined into `otherYield`'s task branch.** No change on arm64; x86's profile put the extra call at 21 instructions per yield.

The self-review then added a guard against merging after a direct wake of the root. Without it a channel send's receiver went on a turn late; a token-test case now covers it. That took back about +2%, so the total is −17.5%.

**Not done.** A lighter turn for a task whose list holds only the root's records was x86's largest item: about 97 instructions per yield across `runOther`, `runEnd` and `fireYields`. It would change what `YIELDS` and `LOCALS` mean for `runEnd`'s requeue and `leftLocals`. I'd do it as a follow-up with its own review, not in this round. With the merge, the second child's turn needs no second poll of the root already. The remaining cost is the record and the list path that a turn per yield needs.

**What a real program pays.** About 160 instructions on arm64 per child yield beyond the first in a poll. Only for an inline `Promise.all`, `allSettled`, `race` or `any` awaited directly in a task's root whose children yield. These are unaffected:
- children that don't yield (fanout_all, all_small_stored flat);
- combinators awaited from a started promise, where `schedAll` gives each child a cx of its own;
- plain root yields (root_yields, root_yields_after_all, hot_loop, all within ±1.3%).

**Asked of the owner:** accept inline_all_yields at this cost as the price of Node's order (`bench_baseline=accept` on the nightly guard once merged), with the lighter turn as a follow-up issue. Or hold this PR for that follow-up.
