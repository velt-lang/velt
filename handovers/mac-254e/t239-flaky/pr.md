Closes #3690.

Two std scheduler tests asserted an order between threads that the scheduler doesn't guarantee. Both now order the events with atomics. No sleeps are used as synchronization, and the scheduler is unchanged.

**`a_carried_wake_still_queued_when_main_ends_is_given_back`** (std_scheduler_wake_order.rs, `exit_queued`):
- **The cause:** the busy task on worker 1 spun 50M iterations after the wake. When the main thread was descheduled for longer than that, `busy true` came before `main ends`.
- **The fix:** it now spins until `main` has printed (`MAIN_DONE`), then a while longer. The wake still leaves the woken task queued behind it when `main` ends, which is the shape the test is for.

**`a_pinned_spawn_runs_its_timers`** (std_scheduler_timers.rs):
- **The cause:** the pinned task's fire-and-forget timer is set when its first 5 ms timer has run. If the pinned task resumes more than about 395 ms late, that timer is due after the worker's 400 ms one. Node prints the same order then.
- **The fix:** the worker's 400 ms timer now starts once the fire-and-forget callback ran (`FIRED`). The pinned task prints its last line after its sleep, once the worker's timer ran (`WORKER_FIRED`). `run_exe`'s one-minute watchdog still fails a run in which a timer never fires.

## How it was tested

On macOS arm64, a debug build, with the Mac's load average between 40 and 65 from other builds. The counts are bad runs out of the total.

| Test | Workers | Before | After |
|---|---|---:|---:|
| `exit_queued` | 4 | 55/200 | 0/300 |
| `exit_queued` | 8 | 22/200 | 0/300 |
| pinned timers | 2, 4, 8 | 0/1800 | 0/900 |
| pinned timers, with a 500 ms stall in the pinned task after its first timer | 2 | 20/20 | 0/20 |

- **`exit_queued`:** every failure before the fix was the order only: `busy true` before `main ends`, every block freed.
- **`exit_queued` still bites:** with `polled`'s give-back removed (sched_wake_away.velt), 60 of 60 runs leak.
- **Pinned timers:** 1800 runs of the old test didn't fail here, 300 of them with 20 `yes` processes running.
  - Over 400 hogged runs, the pinned 5 ms timers ran at most 6 ms late. There's no cluster near 400 ms, so nothing points to a missed wake.
  - A 500 ms stall in the pinned task after its first timer reproduces the CI failure's output exactly on the old test, and the new test passes with it.
- **Cargo:** `cargo test -p veltc --test std_scheduler_wake_order` (6) and `--test std_scheduler_timers` (14) pass.
