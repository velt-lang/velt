**Self-check** (CONTRIBUTING.md "Before opening a pull request"):
- **Node:** the pinned-timers order is now Node's whatever the timing. Node prints the old test's failing order too, when the pinned task resumes more than about 395 ms late.
- **Docs:** none (tests only).
- **Regression tests:** these are the tests. `exit_queued` still fails with the bug it guards (60/60 leak with `polled`'s give-back removed).
- **No sleeps as synchronization:** atomics order the events. The 400 ms and 2 s timers stay as what the test checks, and `run_exe`'s watchdog fails a run in which a timer never fires.
- **Benchmarks:** none (no code change). **Size:** both files are under the limit.

**Checks:**
- `cargo xtask precheck` passes.
- The two test files pass: wake_order 6/6, timers 14/14.
- I haven't run the full gate locally. `cargo xtask affected` selects all veltc tests and every golden, since the files live in veltc, which is a heavy job for two test programs.

**#3722:** this PR doesn't touch #3722's two `cfg_attr` lines, because #3722 isn't on main yet. If #3722 lands first, I'll merge main in and remove both lines here. If this one lands first, #3722 can be dropped.
