Part of #3097 (step 1 and gather-back). Closes #3117.

A call between two workers of the executor in Velt cost a full kernel wake on each hop: the woken worker had just parked in `epoll_wait`/`kevent`. Once step 2 lets tasks spread, a call often crosses workers, so that cost becomes the common case.

- **Spin before parking** (sched_spin.velt). A worker with nothing to run spins briefly on its inbox before it sleeps in its reactor, before its timer look. While it spins its park word stays down, so a waker on another worker only pushes, with no system call, and the worker takes the task without a kernel wake or a context switch.
  - The budget is adaptive: it starts at 20 µs, doubles up to 50 µs after a spin that found work, and halves down to off after one that didn't. A park that ends soon after it began turns it back on at 2 µs, so an idle worker spins a few tens of microseconds once and then sleeps as before.
  - It is used only with several workers, and only while another worker is awake and not spinning itself. A pause (`atomicSpinHint`) between looks leaves the core's other hardware thread its turn.
- **Gather-back** (sched_gather.velt). Tasks that a burst spread over the workers come back to worker 0 once the load is gone. An idle worker (tasks spread, and at least half of its polls in the window, and at least 16, began with a kernel wake) hands the tasks woken onto it back to worker 0, unless worker 0 is loaded, lazy or stalled. An anchored task goes along only when it can carry its sleeps. A busy worker, or one whose spin catches its tasks, is seldom woken from its reactor, so a burst's spread stays in place: this is the hysteresis against offers.
- **#3117.** A spare worker that is asked to retire while a task the rescue moved to it sleeps there now carries the sleep with the task to worker 0 and retires. Before, the spare declined to retire while the sleep was pending (sched_relax.velt `keeps`), so worker 0 stayed shared and never became lazy again.
- **Inlining.** `schedShare`'s forwarding is a loop instead of recursion, and it is `@inline`, as `schedBox` now is. This keeps both inlined into hot loops when a program is split into two codegen units (with #3173).

Tests: `std_scheduler_gather.rs` `actors_a_burst_spread_come_back_to_worker_0`: actors a burst spread to idle workers answer low-rate calls from worker 0 on worker 0 by the end; it fails with gather-back off. `std_scheduler_rescue.rs` `a_task_sleeping_on_a_retiring_spare_goes_to_worker_0_with_its_sleep` covers #3117 (worker 0 stayed shared without the fix). The existing std_scheduler suites pass (235/235).


