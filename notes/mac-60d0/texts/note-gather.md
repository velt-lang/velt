#3202 item (a), the flaky gather test: CAUSE FOUND (instrumented runs on the Mac, main's compiler + the stack's std rebased on main locally).

The test never exercised gather-back. Counters in takeInbox showed ZERO inbox takes with tasks in the whole program, on every worker, in every run. Its actors may move, and a woken task that may move runs in its waker's slot (sched_wake.velt schedule: pushNext on the waking worker, home kept), so no call ever went through an inbox, where gather-back lives. The last round's 'away 0 or 16' only showed where `main` was: the burst's offers sometimes moved `main` itself to another worker, and then all 16 actors ran there (16 away); otherwise all ran on worker 0 (0 away). That is why every failure had all 16 away, why the criterion made no difference (50% kernel wakes, wakes per take, run of idle takes: none of them ran), and why the builder saw 7,135 polls with 69 kernel wakes on 'the actors' worker' (it was main's).

Gather-back only matters for a task that cannot run in its caller's slot: an anchored one (it slept once; ANCHORED), which a cross-worker wake sends to its home's inbox. So the test's workload is now: actors that sleep once (anchored, carryable), and the caller a task pinned to worker 0 (the burst can't move it); the burst runs until a round finds actors away.

With that, on the Mac (program run directly, 4 workers, a gate running beside it):
- WIP criterion (a run of 16 inbox takes each right after the worker was idle): 20 of 20 runs spread (4 to 11 actors away at the first low-rate round) and end with 0 away; counters: every spread actor gathered (gave = spread), nearly every take from idle.
- gather-back off (MIN_TAKES out of reach): 10 of 10 runs end with 4 to 11 away. So the test fails without it, deterministically.

So the WIP's criterion stays (it is reached by the real case), and the fix for (a) is the test's workload. Next: build the stack (rebased on main) on the Mac when my gate for #3236 ends, run the gather test 20x sequential and 20x parallel plus all std_scheduler suites, then push and ask you for the builder rows for (b) (T4 timers_due_stored/_reversed, async rows).
