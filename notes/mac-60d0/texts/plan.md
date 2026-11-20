mac-60d0 PLAN for #126 (handover on #1 and #83 read in full).

1. #3174 leak first (blocks=20/19 in std_scheduler_rescue a_line_printed_on_a_lazy_worker..., ubuntu, run 8).
   - Light, now: read the CI log of run 44402225385 and the test, and work out from the code which block can be left (one allocation, 19 of 20 freed) and whether #3174's diff can reach that path at all (the test program has no server).
   - Then the Linux VM (velt-prof, needs a slot): build velt + velt_rt for head 54bbaf978 and main in VM-local targets, run the test 50x at once and 50x under load (stress beside it), head against main. Cleanup after.
   - Head only: fix on serve-spread with a regression test that fails without it. Main too: file the issue with the repro and fix it first in its own PR.
2. #3202 round 2, on cross-wake-wip (worktree branches/cross-wake):
   - (a) instrumented runs of the gather test's program (counters for inbox takes, idle runs, gathers() outcomes per worker, printed at exit through a debug env switch, not committed) to see why gathers() never fires in the failing runs; then a criterion the workload reaches; 20/20 sequential and parallel, and the test fails with gathers() off.
   - (b) the WIP's re-inlined steal/borrow and spin-only-after-another-worker's-unpark: Mac suites, then ask you for the builder rows (T4 timers_due_stored/_reversed, all async rows), since the VM's cachegrind doesn't run the spin.
3. After the stack: the handed-on sleep anchoring issue (linked from #3201).

mac-36ea: I'll message it now about the sched_* files; my stack touches sched_park/spin/gather/queue/task/spread/relax/sleep_carry. Proposal: #3202 lands first, 9d rebases on it, unless its change is small and ready sooner.

I use the existing worktrees branches/serve-spread and branches/cross-wake (mac-afcb's, wound down) with their own targets.
