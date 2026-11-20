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
