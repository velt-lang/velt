Thanks. Fixed in bdb2eb762 (on ec5d4605d, which rebased this PR onto main with the rest of the stack):

- **R1 (a due sleep loses to a yield):** before a poll fires its yields, the task's sleeps pending on this worker's timer block whose deadline passed move to its due list and fire first (sched_fire.velt `dueNow`, called from `fireYields`). Only this task's sleeps move, and nobody is woken: the worker's own look still moves the others. New golden `lang/yield_now_due_sleep` is p22 plus p8's shape (a started promise's sleep against another one's yield). It is Node's output, the same in 5 Node runs.
- **R2 (use after free between `fire`'s CAS and its waker swap):** new state `FIRING`. `fire` does CAS `QUEUED → FIRING`, swaps the waker out, then a release store of `FIRED`. `storedPoll`'s cross-task path and `storedDrop` wait while it is `FIRING` (`settled`), so the block is freed only after `fire` is done with it.
- **Follow-up 1 (a root's record fired in the poll it yielded in):** `resume` now has the same guard as `between`. With `ROOT_YIELDED` set it doesn't poll the root again. It marks the task `NOTIFIED` instead, so its next poll resumes it.
- **Copilot's note on `yield_now_root_after_turn`'s header:** the comment now says the `main()` call is added for Node.

Also in this push, from the VM measurements:
- The yield list's turns compile only in a program that yields (`stdUsed("yields")`).
- A stored yield polled or dropped in its own task's poll uses plain loads and stores. Its turn runs on that thread, so nothing races with it there; `Y_TASK` records the task.
- When the last turn is the root's, `runWoken`'s own root poll takes it.
- `enqueue`'s buffer growth (sched_micro.velt) is out of line, so the rest of it inlines where it is called. It had stopped being inlined into `race_small`'s step.

Not ready for re-review yet: race_yield is still +13.6% over #3256, and timers_due_reversed +3.9%. Checked so far, in the VM: all six yield goldens at 1 and 4 workers, and the R1 break check (without `dueNow`, `yield_now_due_sleep` prints the immediates first). I will ask for re-review once every row is within ±3% and the full suites have passed.
