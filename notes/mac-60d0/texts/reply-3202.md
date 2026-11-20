Round 2, pushed as one commit on top of the earlier ones (`std: gather-back counts inbox takes with nothing else queued; any wake ends a spin; the gather test's actors are anchored`). The branch is rebased on #3201's current head (main merged in, `sched_run.velt` split), so it was force-pushed.

**(1) The gather-back test was flaky.** It never exercised gather-back. Counters in `takeInbox` showed no inbox take with tasks in the whole program: the test's actors could move, and a woken task that may move runs in its waker's slot (sched_wake.velt `schedule`), so no call passed an inbox. The last round's "0 or 16 away" only showed where `main` was, which the burst sometimes moved. Changes:
- The test: actors sleep once (anchored, so a call from another worker reaches them through their worker's inbox), the caller is a task pinned to worker 0, and the burst lasts until a round finds actors away.
- The criterion: 16 takes of the inbox in a row that each find nothing else queued on the worker (`q.ownEmpty`, under the lock the take holds). The earlier "right after a park or a spin" was a race with the caller's next call even on a quiet machine and failed under load on Linux; the queue test has no timing in it.
- Results: 20/20 one after another and 20/20 at once on macOS and on the Linux builder (load 42); with gather-back off the test fails every time (4 to 11 actors stay away).

**(2) T4 `timers_due_stored` / `timers_due_reversed` over 3 %.** `steal` and `borrow` are `@inline` into the idle loop again, and a park turns the spin back on only when another worker ended it. Builder: +0.18 % and +0.15 % (T1 −0.60 / −0.58).

**Found while re-measuring:**
- `await_deep` +9.8 %: sched_mem.velt `copy` copies every async call's initial state and was left a call across a codegen-unit boundary; it is `@inline` now (builder: 0.00 %).
- A wake that brought no task was dropped by a spinning worker (its park word was down, and the spin watched only its inbox): `std_scheduler_bounds` `a_task_ending_last_on_another_worker_frees_its_block_first` failed 8 of 12 under load. The spin is now a state of the park word (`PARK_SPIN`); `unpark` takes a spinner's word down with a CAS and no system call, and the spin watches the word. After: 12/12 on the builder at load 40. `E_SPINNING` is gone.
- `await_chain` +3.56 % on x86-64 is not from this branch's code: with one codegen unit `main`'s poll function is byte-identical with and without the branch. The program crosses the 32 000-statement unit bound with the branch's std, and its entry file's code then sits alone in a unit. That is #3252's subject, where the row is decided.

Checked on macOS: all std_scheduler suites (30 test binaries, gather, carry, bounds and rescue included), `velt fmt`, std-lint, `cargo xtask precheck` on the earlier tree; instruction counts on arm64 within ±1.2 % for the async rows measured (timer_await −5.3 %). No full gate rerun after the squash (the tree is the one the builder measured, 547ba9f10).
