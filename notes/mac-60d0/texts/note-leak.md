#3174 queue failure: REPRODUCED ON MAIN, root cause found, fix written (not #3174's).

Repro (Linux VM, main 0eb960cec, the test's program run directly, 16-18 loops at once on 9 cores): 5 of 26,000 runs end with blocks=20/19 (and 21/20 with two spares). The test itself 50 at once: 0 of 400 (8,000 program runs), so it needs the direct loop to show. The program uses no net code; #3174 is not involved.

Cause (a temporary probe in velt_rt named the block: 144 bytes, made on the monitor's thread, freed within 100 ms after the report): std/lang/hooks/thread_posix.velt startThread takes a COUNTED block (m.zeroed) for the pthread attributes and frees it after pthread_create. The stall monitor starts a spare with it (sched_monitor startSpare); when the monitor's thread is taken off its core between pthread_create and the free, the spare runs the rescued task to its end and the program takes its block counts first. Not a lost block: a late free on a thread the counts don't wait for. macOS has the same code; the Mac's timing hides it.

Fix (branch rescue-leak, on main): the room comes from the C library (m.lastingZeroed / lastingRelease, outside the block counts, as the executor's other blocks that live on threads the counts don't wait for). With it: 0 of 27,000 runs in the same VM loop, and the probe shows no counted block made off the main thread any more.

Regression test (std_scheduler_rescue starting_a_spare_counts_no_block): a program that needs one spare, or two with DEEP=1, with the same tasks either way; the blocks made must be equal. In the VM: with the fix 20 and 20 (200 runs each, spares=1 / spares=2 every run); on main's std 22 and 23.

Next: VM cleaned; Mac build of rescue-leak in my slot, the rescue suite, the test against the unfixed std, precheck + check.sh, then the PR (small, its own, as you said for a main leak). #3174 can re-queue once it merges.
