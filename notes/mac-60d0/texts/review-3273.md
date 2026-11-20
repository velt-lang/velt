## REVIEW COMMENTED
<!-- ccr-overview-v2 -->

### 🟡 Changes recommended

The change still has unresolved correctness and concurrency hazards—critical atomicity/order violations around cross-worker `S_STATE` access and a test that double-waits on a child process after a timeout—which could cause races or mask failures.

<details open>
<summary><strong>3 open issues</strong></summary>

- <picture><source media="(prefers-color-scheme: dark)" srcset="https://github.githubassets.com/static/images/icons/copilot-code-review/high-v2-dark.svg"><source media="(prefers-color-scheme: light)" srcset="https://github.githubassets.com/static/images/icons/copilot-code-review/high-v2-light.svg"><img src="https://github.githubassets.com/static/images/icons/copilot-code-review/high-v2-light.png" alt="High severity" width="62" height="18" align="texttop"></picture> [Shared S_STATE is still written non-atomically by seenOn](https://github.com/velt-lang/velt/pull/3273#discussion_r4628331789) · New
- <picture><source media="(prefers-color-scheme: dark)" srcset="https://github.githubassets.com/static/images/icons/copilot-code-review/high-v2-dark.svg"><source media="(prefers-color-scheme: light)" srcset="https://github.githubassets.com/static/images/icons/copilot-code-review/high-v2-light.svg"><img src="https://github.githubassets.com/static/images/icons/copilot-code-review/high-v2-light.png" alt="High severity" width="62" height="18" align="texttop"></picture> [release() mutates S_STATE before the final release store](https://github.com/velt-lang/velt/pull/3273#discussion_r4628331849) · New
- <picture><source media="(prefers-color-scheme: dark)" srcset="https://github.githubassets.com/static/images/icons/copilot-code-review/medium-v2-dark.svg"><source media="(prefers-color-scheme: light)" srcset="https://github.githubassets.com/static/images/icons/copilot-code-review/medium-v2-light.svg"><img src="https://github.githubassets.com/static/images/icons/copilot-code-review/medium-v2-light.png" alt="Medium severity" width="62" height="18" align="texttop"></picture> [Timeout path performs a second wait on the child process](https://github.com/velt-lang/velt/pull/3273#discussion_r4628331899) · New
</details>

<details>
<summary><strong>What changed in this PR</strong></summary>

Fixes a scheduler concurrency bug where `sleep` futures could be polled/dropped on a different worker than the one holding their timer-block state, causing cross-worker races and crashes.

**Changes:**
- Introduce a cross-worker sleep handover/request mechanism so only the holding (“home”) worker mutates pending sleeps.
- Extend sleep metadata/state and integrate request serving into tick/park/shutdown paths to guarantee progress and clean exit.
- Add counted-run regression tests to reliably reproduce and validate the previously flaky failure mode.

| File | Description |
| ---- | ----------- |
| std/​lang/​hooks/​sched.velt | Ensures outstanding cross-worker sleep handover requests are drained during scheduler shutdown/finish. |
| std/​lang/​hooks/​sched_tick.velt | Serves pending sleep-handover requests during I/O ticks to guarantee forward progress while the worker is busy. |
| std/​lang/​hooks/​sched_task.velt | Adds a per-worker `E_SLEEP_ASKS` request stack field (and related layout sizing) to support cross-worker sleep handover. |
| std/​lang/​hooks/​sched_sleep.velt | Updates sleep poll/drop/expire logic with home-worker tracking and routing so non-home access delegates via the handover path, plus counted-run invariant checks. |
| std/​lang/​hooks/​sched_sleep_move.velt | Implements the cross-worker sleep handover protocol (request posting/serving/settling and state transitions). |
| std/​lang/​hooks/​sched_sleep_forget.velt | Factors sleep-forget/cleanup behavior into a dedicated module to isolate ownership and error-path handling. |
| std/​lang/​hooks/​sched_sleep_carry.velt | Adjusts offer/carry behavior so sleeps moved with tasks are marked appropriately and re-homed on adopt. |
| std/​lang/​hooks/​sched_park.velt | Integrates serving of sleep-handover requests into the pre-timer and pre-park flow and avoids parking while requests are pending. |
| std/​lang/​hooks/​sched_exit.velt | Wires uncaught-error exit cleanup to the new sleep-forget module. |
| std/​lang/​hooks/​sched_end.velt | Treats pending sleep-handover requests as active work so program end waits for request drainage. |
| crates/​veltc/​tests/​std_scheduler_sleep_handed.rs | Adds regression tests covering cross-worker await/drop/offer scenarios for sleeps under counted runs with allocator checks. |
</details>

<sub>Review effort: Balanced</sub><br><sub>Copilot was unable to complete this review at the selected tier due to model policy or plan restrictions.</sub>
## REVIEW COMMENTED
# Review: #3273 std: a sleep polled or dropped on another worker is handed over by the worker that holds it (#3207)

Head **f50099f9d** (base main 8061c8631). Platform change, full depth. All runs were on the Linux
builder: debug build of the head, runs counted (`VELT_RC_STATS=1`) with the checking allocator
(`VELT_RT_DEBUG_ALLOC=1`), loaded by running 6-8 at once (builder load 30-100 from other jobs as
well). Perf rows are not measured here: the maintainer is doing the timer rows with #3252.

## Summary

The design is the right one, and the head fixes what #3201's review found:
- the routing through the holding worker;
- `S_HOME` stored before a registration's state (release);
- the state stored last on a hand-over;
- requests counted in the sleep;
- served before the timer look and at the park's re-check;
- the end waiting for the requests.

Under load the head ran handed4/handed5 1,200 times at 1, 4 and 8 workers with no failure, and the
two mutants are caught (below).

But two races remain in the hand-over protocol, and I can show each one:
1. **A transient `NEW` state.** I widened its window and got failures; separately, the unwidened
   head crashed with a SIGSEGV that matches this race.
2. **A use-after-free window in the request count.** Also shown with a widened window.

There are also silent 60 s timeouts at about 1% in two stress shapes, and the retired spare never
serves the new list.

## Required changes

### 1. `unlinkPending`/`unlinkDue` store `NEW`, which another worker can read as "handed over"

**Files:** `sched_sleep.velt`: `unlinkPending` at line 297, `unlinkDue` at line 333.

**The race.**
- Both functions end with `m.st(f + S_STATE, NEW)`.
- `expire` (line 397), `carried` (line 366) and `fire` call them and only then store the state they
  mean (DUE, SEEN, CARRIED, FIRED).
- In between, the sleep reads as `NEW`. Since this change, that is the state a hand-over leaves for
  the asker (`release`).
- A task polling the handed-on promise on another worker in that window takes `sleepPoll`'s first-
  poll path. It writes `S_WAKER` (dropping the old waker the expiring worker is about to wake),
  `S_OWNER` and `S_HOME`, and inserts the sleep into its own block, while the holder puts it on a due
  list or an offered task's list.
- The rule "a poll or drop from another worker writes nothing in the sleep" fails exactly there.

**Evidence.**
- **Widened window.** A std copy with a 4,000-load loop after `unlinkPending` in `expire` and
  `carried` (variant `wide`), at 4 workers, 30 runs each (6 at once):

  | probe | failures | of which |
  |---|---|---|
  | handed4 | 12/30 | 10 SIGSEGV, 2 debug-alloc aborts |
  | handed5 | 13/30 | 5 `ICE: a sleep expires for a task on another worker…`, 5 debug-alloc, 3 SIGSEGV |
  | handed7 (two tasks await one handed sleep) | 19/30 | |

  The same widened loop, with the two `NEW` stores removed (`widefix`): handed5 30/30 clean,
  handed4 and handed7 29/30 each, with one silent timeout each (see item 3).
- **Unwidened head.** I ran handed4 at 8 workers 200 times under gdb (no breakpoints). One run
  stopped on a **SIGSEGV in `expire` -> `m.wakeOwned(w)`** (sched_sleep.velt:404, the non-task waker
  branch, right after `unlinkPending`). That is this race: the waker read there had been dropped by
  a concurrent first-poll path on another worker.
- **Unwidened, `NEW` stores removed** (`fixnew`), same gdb hunt: 250/250 clean.

**Fix.** Make `unlinkPending` and `unlinkDue` leave the state alone. Every caller already stores the
state it means, or frees the sleep:
- `release` stores its final state with a release store;
- `expire` stores DUE via `pushDue`, or SEEN;
- `fire` stores FIRED;
- `carried` stores CARRIED;
- the `firesItself` path stores SEEN;
- `sleepDrop` and `forget` free the sleep.

Then `NEW` is only ever stored at creation and by a finished hand-over. I'd also make `expire`'s,
`fire`'s and `carried`'s final stores `storeRelease`, as `insert`'s is, since remote pollers `load`
the state.

### 2. `one()` reads the sleep after its decrement, which can free it from the asking side

**File:** `sched_sleep_move.velt` `one`:
`if (m.dec(f + sl.S_ASKS) === 1 && isGone(f)) free(f)`.

**The race.**
1. The decrement makes the count 0.
2. Then `isGone` reads `S_STATE`.
3. In between, the asking task can drop the promise on its own worker. `sleepDrop` reads
   `S_ASKS === 0`, sees `NEW` (the hand-over just stored it) and frees the sleep locally.
4. The holder then reads freed memory.
   - With the checking allocator that is poison and harmless.
   - In a release runtime the block may already be reused. If the word at `S_STATE` happens to be 6
     (`GONE`), the holder frees someone else's block.

**Evidence.** A copy that widens that window and checks the debug allocator's FREED header before
the read (`widedec`): handed4 at 4 workers, 5/30 runs end with
`ICE: probe: a served sleep was freed under its last request`. handed5 and handed8 were 30/30 clean.

**Fix.** Make "last request ended and the sleep is dropped" one atomic decision. For example:
- keep a `GONE` bit in the `S_ASKS` word: the dropper does an atomic OR and frees only when the
  count part was 0; a server does an atomic subtract and frees when the result is exactly the bit;
- or have `sleepDrop` take part in the count whenever the sleep was ever registered on another
  worker.

**Test.** Add a mutant check for it: a regression test that fails when the dropper and the last
request race.

### 3. Silent 60 s timeouts in the stress shapes: explain and fix (probably items 1 and 2)

No output and nothing on stderr, killed by the cap (normal runs take 1-6 s):

| what | timeouts |
|---|---|
| head, handed7 at 4 workers | 2/100 |
| head, the PR's own test file 12 at once | 1/12 (`handed_on_sleeps_fire_under_offers`, "run 2 at 8 workers": `left: None`) |
| `fixnew`, handed4 at 4 workers | 1/100 |
| `widefix`, handed4 and handed7 | 1/30 each |

- These are not slow runs. With a 300 s cap and the debug view on, 500 runs never took more than
  6 s.
- I couldn't catch one in the act. The only stop under gdb was the SIGSEGV of item 1. A crash in a
  worker while the checking allocator's fault reporting holds a lock would look like this.
- After 1 and 2, the loaded stress should show zero, with each run's time recorded:
  - the test file 12 at once, 3 rounds;
  - handed7 (two awaiters) at 4 workers x 300;
  - handed4/5 at 8 workers x 300.

  Add the two-awaiter shape (handed7.velt) to `std_scheduler_sleep_handed.rs`. It is valid
  TypeScript, and on main it crashes.

### 4. A retired spare never serves its list

**File:** `sched_relax.velt` `retiredLook`.

- It parks after checking only the inbox, queue and retirement state. It never calls `mv.serve`,
  and `keeps()` does not count a non-empty `E_SLEEP_ASKS`.
- So a request that reaches a spare that has retired is never served:
  - the asker is never woken;
  - `UNDER_WAY` never reaches 0, so `settle` spins forever at the end;
  - `workerBusy` says busy forever.
- How a request can get there: the sleep's `S_HOME` stays the old worker after a hand-over, and a
  late request or a drop in transit goes there.
- I couldn't reproduce it: 160 rescue runs (`VELT_THREADS=1 VELT_RESCUE=1`, handed4/5/7/8) were
  clean. It's a missing branch on the counted-exit path.
- **Fix:** serve the list in `retiredLook` (before handing on, and in its park re-check), and decline
  to retire while the list is non-empty.

### 5. Tests for the design points that have none

The PR's tests cover the routing (pinned poll, pinned drop) and the review's stress shapes. Add:

- **Expiry while a request is queued.** The holder finds the sleep DUE or FIRED in `release`. For
  example, a pinned worker-1 task polls `sleep(1)`, worker 1 stays busy for about 20 ms, then the
  worker-2 poll arrives. That exercises `unlinkDue` and `seenOn` in `release`. Today nothing forces
  those branches.
- **A third worker in transit:** the handed7 shape (item 3).
- **A carried sleep with a request waiting** (`found === 0`): an offered task's sleep polled by
  another task before the steal.

## Checks that passed

**Probes on the head, under load, counted, checking allocator** (fired counts exact, blocks
balanced, no debug-alloc message):

| probe | 1 worker | 4 workers | 8 workers |
|---|---|---|---|
| handed4 | 200/200 | 200/200 | 200/200 |
| handed5 | 200/200 | 200/200 | 200/200 |
| handed6 (new: the handed-on poller is offered after anchoring with the sleep) | | 100/100 | 100/100 |
| handed7 (new: two tasks await one handed sleep) | | 98/100 (2 timeouts, item 3) | 100/100 |
| handed8 (new: the last drop happens off-worker with a request in flight; must end promptly) | | 100/100 | 100/100 |

- With the debug view and a 300 s cap: handed7 at 4 workers 400/400 (two runs of 100 and 300);
  handed4/handed5 at 8 workers 100/100 each.
- Rescue (one worker plus spares): handed4/5/7/8 40/40 each.

**The "expires off its home" check.** The PR's counted `ICE` is the same check as my #3201 copy.
It never fired on the head. I added another one: an `ICE` when a sleep's *owner* polls it off its
home, the path that skips the hand-over. 150/150 clean.

**Mutants:**

| mutant | probes | the PR's test file, 12 at once |
|---|---|---|
| routing removed (poll and drop never ask) | handed4 16/30 and handed5 4/30 `ICE: … expires …`, handed8 21/30 `ICE: … dropped …` | **0/12 pass**: both pinned tests ICE |
| `S_ASKS` ignored by a drop | handed5 4/30 `use after free` | **1/12 pass**: `handed_on_sleeps_fire_under_offers` fails in 11 of 12, `a_sleep_awaited_on_another_worker_moves_to_it` in 2 |

**Lost unpark at park: fine.** `E_SLEEP_ASKS` is re-checked after the park word goes up, against
`send`'s push-then-`unpark`. The ioTick and park-entry serves are fine.

**Wakers:**
- A request's waker clone is consumed by `wakeOwned`, kept while forwarded or re-queued, and dropped
  by `release` on a drop.
- The registered waker is woken or dropped in `release`.
- Every counted run ended balanced.

**Standards:**
- No contract files touched (`E_SIZE` and the sleep layout are internal).
- Commit by the configured author, no trailers.
- The test file has its `timing:` comment.
- sched_sleep.velt is 440 lines including doc comments. std-lint counts lines besides doc comments,
  and the author reports it passes; I did not run std-lint.

## Follow-ups (not blocking)

- **A dead check.** `sleepDrop`'s `if (wk.counting() && m.ld(f + S_HOME) !== e) held("is dropped")`
  can only fire when the routing above it is removed, which is what the mutant relies on. A comment
  saying so would help.
- **A carried request spins.** A request for a carried sleep is pushed back on the worker's own list
  without a wake. The park re-check then keeps that worker from sleeping until the offered task is
  adopted (a short spin). That's fine as long as offers are stolen quickly; worth a comment.

Scripts, probes and raw output:
- builder ~/velt-build/r3273 (kept for the re-review): `runs/`, `hang/`, `gdb/`, `tests/`,
  `batch*.log`;
- the scripts: D:/velt-build/r3201/r73*.sh and the scratch copies;
- the probe programs: D:/velt-build/r3201/probes/handed6.velt, handed7.velt and handed8.velt.

Verdict: CHANGES NEEDED

## COMMENT
Self-check (CONTRIBUTING "Before opening a pull request"):

1. **Node**: n/a, no golden added or changed. Node has no workers; the behaviour here is which worker touches a timer.
2. **Docs**: no user-visible change (a crash and a hang fixed; no API or semantics change).
3. **Tests**: `std_scheduler_sleep_handed.rs`. With the two routing checks taken out (a copy of std whose poll and drop never ask the holder), `a_sleep_awaited_on_another_worker_moves_to_it` fails with `ICE: a sleep expires for a task on another worker than the one that holds it` and `a_sleep_dropped_on_another_worker_leaves_its_block` with `ICE: a sleep is dropped on another worker than the one that holds it`, 6 of 6 runs each; with the fix 6 of 6 pass. The stress test is the review's two probes (#3207).
4. **Panics**: the two `ICE:` panics are invariant checks, in counted runs only.
5. **Performance**: timer rows, instructions retired, macOS arm64, min of 3, same compiler, base = main's std before this change (1 / 4 workers): timers +1.1 / +1.2 %, timers_due_stored +1.6 / +1.5 %, timers_due_reversed +1.6 / +1.4 %, timers_due_join +1.2 / +1.2 %, timers_due_in_promise +1.4 / +1.3 %; race_select +0.1 %. The x86-64 rows are being measured on the builder and will be posted here by the maintainer.
6. **Timing**: the tests order events by awaits and pinned workers; the sleeps in them are the thing under test (a 20 ms sleep that must fire on the other worker; a ten-minute one that must not keep the program). `cargo xtask repeat` covers Velt test files and goldens, not this Rust test, so I ran the stress test 8 at once, 5 rounds: 40 of 40 pass.
7. **Size**: about 780 changed lines, 334 of them the test file (four probe programs).
8. **std**: no API changed; std-lint passes (sched_sleep.velt stays within 400 lines: the holder's side of the hand-over and `forget` are in files of their own).

How it was found and checked:
- Three instrumented copies of std (an `ICE:` on a sleep that expires off its task's home, is polled off its block's worker, is dropped off it) showed which shapes the review's probes reach on main: the first in 1 of 10 runs, the second in 2 of 10, and segfaults in 5 of 20.
- With the fix, in counted runs with the checking allocator: the two probes at 4 and 8 workers 160 of 160 clean and balanced; in a loaded loop (8 at once) 0 hung and 0 failed of 640.
- Two defects of earlier versions of this change were found by running the stress under load and are fixed here: a late request took a re-registered sleep off its list and woke only a finished combinator's waker (a hang, 5 of 96 loaded runs); a drop request was still on a list when the last task ended (the block count off by one, once). A shortcut that read the polling task from the promise's waker or context was wrong (it names the promise's owner) and is not in the change.

Gate: before the rebase onto today's main (on #3201's branch): `cargo xtask precheck` passed; the std_scheduler suites and c_export_async under nextest 249 of 249. After the rebase the tree is the same change on main (a clean rebase); the suites on the new main are pending a build slot and I'll post them here. No full `scripts/check.sh` on the Mac: the merge train runs it.

## COMMENT
Update to the gate line above: the maintainer decided to skip a local run on the rebased main; this pull request's CI and the merge train run the suites.
