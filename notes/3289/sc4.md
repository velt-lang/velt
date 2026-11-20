Self-check (CONTRIBUTING.md "Before opening a pull request"), at 4ce3db55e. This PR is stacked on #3256 (branch yield-order-3241), which is stacked on #3250 → #3249 → #3187. I'll retarget it as those merge.

- **Node comparison:** `yield_now_immediates` and `yield_now_root_after_turn` are Node's output for the same programs (`yieldNow` = a promise `setImmediate` resolves, `sleep` = a `setTimeout` one). Both were the same in 3 Node runs.
- **Docs:** user-visible change: stored yields and racing yields/sleeps now take their turns in Node's order. The docs don't describe this order, so there's nothing to correct. I'll file a docs-sync issue if you want it stated.
- **Regression tests:** each piece fails the new golden when broken (details in the description): stored yields not queued, no woken promises between turns, `fireDue` without the between-turns step, and the `ROOT_YIELDED` guard in `between` (`yield_now_root_after_turn` stops after `1`).
- **No unwrap on user input:** std only, nothing on user input.
- **Benchmarks:** the coordinator measures on the Linux builder (rows and threads in the description). #3256's `@inline` and R1 numbers (spawn_many, race_yield) come from the same run, on #3256.
- **No sleeps. Size:** std only; sched_fire.velt is new, and every file is under 400 lines (std-lint).
- **Checks (the train runs the full gate):**
  - full goldens pass;
  - all 27 std_scheduler suites pass, and 5 of them at 8 workers;
  - std's own tests pass.
  - At `VELT_THREADS=1`, std/timer_spawned_busy_first and std/task_spare_worker_index time out. They need a second worker by design and do the same on #3256's std.
  - One-off: a 1-block leak in std/fs_race_settled_first (release, 8 workers), 0 of 12 reruns (the description has more).
