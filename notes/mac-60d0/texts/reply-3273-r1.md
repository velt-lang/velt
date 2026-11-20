## Review round 1: changes

Head **7f5cfbf5d** (two commits on f50099f9d: e5cea522c, the review fixes; 7f5cfbf5d, four dead stores out). Base unchanged (main 8061c8631).

**(1) Transient NEW.** `unlinkPending` and `unlinkDue` no longer store a state; every caller stores the state it means (or frees the sleep). The ordering argument for the holder's own states is in `insert`'s doc comment: `PENDING` (which publishes the registration: `S_OWNER`, `S_HOME`, the links) and `SEEN` (after which a reader may free the sleep) are released; `DUE` and `FIRED` are plain stores, since a reader that sees either only asks the holder.

**(2) Use-after-free in `one()`.** Whether the sleep was dropped (`GONE`) is read *before* the count goes down, and the sleep isn't touched after the decrement.

**(3) Loaded stress, with the two-awaiter shape.** After the fixes and after the ordering change, under load: handed4 and handed5 at 8 workers and handed7 (two awaiters) at 4, 300 runs each: 0 hung, 0 bad. The test file run 12 copies at once, three rounds: clean. handed7 is now in the test file (`HANDED_TWO` in `handed_on_sleeps_fire_under_offers`).

**(4) Retired spare.** `retiredLook` serves its request list and counts it in `keeps()` before it parks (sched_relax.velt).

**(5) Missing tests.**
- *A third task polling in transit:* `a_sleep_awaited_from_three_tasks_on_other_workers_wakes_them_all`. Three tasks, two on worker 2 and one on worker 0, await a sleep worker 1 holds. It fails every run with the routing disabled (the rule's `ICE:`).
- *Expiry while a request is queued (DUE/FIRED in `release`):* I couldn't reach it from a program. A sleep inside a promise is polled only by that promise's task (sched_node.velt `run`), so it never travels with a request outstanding while it expires. Probes with a panic on each state in `release` never fired for DUE or FIRED in hundreds of runs. The DUE/FIRED branches stay mutation-checked by reading. If you know a shape that reaches them, I'll add it.
- *Carried sleep with a request waiting:* covered by `handed_on_sleeps_fire_under_offers` (offers between polls); `release` answers 0 for `CARRIED` and the request is re-pushed.

**Also fixed on the way:** `expire`'s path for a waker that isn't a task's stored `SEEN` before zeroing the waker. A drop on another worker that sees `SEEN` frees the sleep and drops the waker it finds: a use-after-free, or a waker dropped twice. The waker is zeroed first now.

**Poll path.** "The owner polls" is decided from the poll's context (`cx[0] - T_WORD`) instead of reading the worker's current task. Evidence that this matches the worker's current task: an assertion of exactly that (context names the owner ⇒ the worker polls the owner) never fired in the scheduler suites, the goldens, the probes or the stress. Reading the code agrees: a started promise is polled only by its `N_TASK` with `N_CX`, and combinators pass their poller's word on.

The expiry rule check moved out of `expire`'s loop into a pass that runs only in counted runs (sched_sleep_rule.velt).

### Rows

**x86-64** (builder, cachegrind, at e5cea522c, base main 8061c8631, same compiler, std swapped; measured by the coordinator). All within ±3%. Worst: due_reversed +2.13% T1; due_join +1.97%/+1.80%; due_stored +1.92%; due_in_promise +1.78%/+1.72%; timers ~+0.95%; fire +1.11%; race_select +0.01%; timer_await T4 −2.82%. 7f5cfbf5d only removes stores, so these can only go down; a re-measure is welcome.

**arm64** (OrbStack VM, cachegrind, one codegen unit, same compiler, std swapped, base main 8061c8631):

| row | e5cea522c | **7f5cfbf5d** |
|---|---|---|
| timers_due_join T1 / T4 | +3.04% / +2.76% | **+2.55% / +2.31%** |
| timers_due_stored | +1.98% / +1.73% | **+1.79% / +1.56%** |
| timers_due_reversed | +2.11% / +1.86% | **+1.96% / +1.73%** |
| timers_due_in_promise | +2.46% / +2.37% | **+2.14% / +2.06%** |
| timers | +0.83% / +0.81% | **+0.68% / +0.66%** |
| timers_fire | +0.95% / +0.80% | **+0.95% / +0.93%** |

Per sleep, what's left on arm64 is the two release stores (`stlr` needs its address in a register), the drop's check for a request under way, the poll's owner check and the home word. Each was measured on its own with a variant.
