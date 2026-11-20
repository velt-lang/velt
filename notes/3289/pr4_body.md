Closes #3241 (second part: yields and sleeps take their turns as Node's immediates and timers do).

#3256 made an awaited `yieldNow()` wait for the microtasks queued before it. Four differences from Node remained, reported on #3241 and in #3289's review:
- **Creation order:** stored yields settled in the order they were first awaited, not the order they were made. `y2.then(...)` before `y1.then(...)` printed `y2, y1`; Node prints `y1, y2`.
- **Two yields racing:** both `.then` callbacks ran before the race's continuation. Node runs the continuation and its microtasks between the two immediates.
- **Two sleeps racing:** the same, between two timers.
- **A due sleep against a yield:** a sleep that came due while its task ran fired after the task's yields. Node's timers phase comes before its check phase.

This PR changes the executor in Velt (std only).

### A yield list per task
- **The list.** A task's yields wait for their turn on a list, in the order they were made (std/lang/hooks/sched_yield_list.velt). The list lives in a yield block that the task makes when it first needs one and frees when it ends. The task block grows by one word, `T_YIELDS`.
- **Awaited yields.** An awaited yield of a started promise or of a join's child puts a record with its waker on the list. A started promise that yielded is flagged `YIELDING` until its turn, so its owner doesn't adopt it meanwhile.
- **The root.** A root that yields turns the task's state to `YIELDED`: the task is queued again, the yield stays suspended if this poll polls the root again for its microtasks, and the next poll's `RUNNING` lets it go on. It needs no record while the list is empty. Flag `ROOT_ALONE` then gives it a record first if yields made after it join the list in the same poll, so the root still resumes first.
- **Stored yields join late.** A stored `yieldNow()` doesn't go on the list when it's made. It gets a stamp: its task's yield generation and a number that grows in the order yields are made.
  - **Polled in a later generation:** its turn has passed, and it's ready at once. A generation is one more in each poll after one that made stored yields (flag `FRESH`).
  - **Polled in the poll that made it:** it joins the list by its stamp, or takes the root's fast path when the root is what waits for it.
  - **Polled by another task:** its turn becomes that task's, from that task's poll on. No task reads another's yield block.
  - **Unpolled or raced away:** a race's loser that was never polled is freed with no list work.

### Turns one at a time
- **Order within a poll.** The task's next poll fires its due sleeps, then its yields made before the poll, one at a time (std/lang/hooks/sched_fire.velt; `fireDue` moves there from sched_run.velt).
- **Due sleeps first.** Before the yields, the task's sleeps whose deadline has passed move to its due list and fire first (`dueNow`), since the worker moves due sleeps only when it looks.
- **Between turns.** After each turn's waiter runs, the task's microtasks run, then the started promises they woke, then the root if it woke. Node runs microtasks between two timers' callbacks and between two immediates the same way.
- **Yields made during the pass** wait for the next poll, as immediates set in Node's check phase do. When the last turn is the root's, `runWoken`'s own root poll takes it.

### Lifetime, threads, cost
- **Task lifetime.** A task with yields on its list is queued again, and doesn't finish until they've had their turn, so `spawn(yieldNow())` still completes.
- **Thread safety.**
  - A stored yield on the list may be polled or dropped by another task. Its state changes by CAS and its waker by swap.
  - A turn goes through `FIRING`, so a holder never frees a yield that `fire` still touches.
  - A stored yield polled or dropped in its own task's poll uses plain loads and stores. Its turn runs on that thread, so nothing races with it there.
  - A yield polled by its task's root holds no reference to the task.
- **Cost.**
  - Only a program that yields compiles the list's turns (`stdUsed("yields")`).
  - The root's awaited yield is in line where it is awaited: for a task with no yield block, one test and one store. The yield hooks know a task's own `cx` by its address (`cx === waker + T_CX`, sched_task.velt) instead of loading the waker's table; a task's waker in any other `cx` takes the table test, out of line.
  - The step between two turns is in line: a turn with no reaction is four loads. `drain` is called only when microtasks are queued or rejections wait (sched_micro.velt `pending`); the woken promises and the woken root are out of line.
  - A poll begins a yield generation off the straight path (`FRESH` is in `run`'s one flag test).
  - `enqueue`'s buffer growth (sched_micro.velt) is out of line, so the rest of it inlines where it's called.
- **wasm.** wasm32 keeps the runtime's `yieldNow` (#3307), so the new goldens join `UNSUPPORTED` in wasm_goldens.rs, as `yield_now_order` did in #3256.

## Tests
Each golden is Node's output for the same program, with `yieldNow` a promise that `setImmediate` resolves and `sleep(ms)` one that `setTimeout` resolves. Each was the same in 3 to 5 Node runs.
- **`lang/yield_now_immediates`:**
  - stored yields awaited in reverse order;
  - two yields racing, then the race's continuation and its microtask;
  - a yield racing a microtask chain, and a started promise that yielded;
  - two sleeps racing;
  - each timer's microtasks before the next timer.
- **`lang/yield_now_due_sleep`:** a sleep that came due while its task ran fires before the task's yields, whichever was made first. Also a started promise's sleep against another promise's yield.
- **`lang/yield_now_turn_passed`:**
  - a stored yield first awaited two turns after it was made is ready at once;
  - a stored `Promise.all` over a yield;
  - the root's turn keeps its place when a started promise yields after it in the same turn.
- **`lang/yield_now_root_after_turn`:** the root resumes during a started promise's turn and yields there.
- **`lang/sleep_turns_root_between`:** two sleeps due in the same poll; the root woken by the first one's reactions goes on before the second fires.
- **Break checks, each on a copy of std:**
  - no root record (`ROOT_ALONE`): `yield_now_turn_passed` differs;
  - no `dueNow`: `yield_now_due_sleep` differs;
  - no lazy join: `yield_now_immediates` and `yield_now_turn_passed` differ;
  - no generation rule: `yield_now_turn_passed` and `yield_now_stored` hang;
  - a root's yield always ready: `yield_now_order` differs;
  - no `drain` between turns, or no woken promises between turns: `yield_now_immediates` differs;
  - a woken root left for the next poll: `sleep_turns_root_between` differs.
  - Without the own-`cx` test the output is the same: the table test behind it gives the same answer.
  - The `FIRING` state guards a window of a few instructions that no program here reaches deterministically, as the review found.
- **Still passing:** `yield_now_order`, `yield_now_stored` and `yield_now_stored_sync` (#3256), and the yield, sleep, timer, immediate, promise, race, spawn, task, catch_panic and uncaught goldens, at 1 and 4 workers.
- **Known at one worker:** `std/timer_spawned_busy_first` fails, as it does on main; it needs a second worker by design.

## Benchmarks
Instructions retired (cachegrind), bench/async rows, at 1 and 4 threads (`T1 / T4`). Two baselines: #3256 (this PR's base, ff566fe4f) and the 9d stack's merge base on main (3b08c525d, where `yieldNow` is the runtime's; `race_yield.velt` copied in). arm64 is the Linux VM on the Mac (std swapped on #3256's compiler; T4 is the minimum of 4 runs for spawn_many, timer_await, race_select and the pingpong rows, one run otherwise). x86-64 is the Linux builder (std swapped on this PR's compiler; minimum of 2 runs at T1, of 3 at T4).

| row | arm64 vs #3256 | arm64 vs stack base | x86-64 vs #3256 | x86-64 vs stack base |
|---|---|---|---|---|
| `spawn_many` | +0.14% / -0.25% | +2.56% / +1.81% | +0.30% / -0.99% | +2.88% / +1.69% |
| `race_select` | +0.64% / -1.05% | +1.48% / +0.74% | +1.46% / +1.22% | +2.30% / +1.67% |
| `race_yield` | +10.19% / +9.43% | -13.72% / -13.41% | +13.23% / +11.97% | -12.12% / -11.87% |
| `race_small` | -4.23% / -4.21% | | -5.02% / -5.00% | -4.77% / -4.75% |
| `pingpong_colocated` | +0.34% / +0.34% | +0.37% / +0.44% | +0.45% / +0.30% | +0.53% / +0.57% |
| `pingpong_pinned` | +0.41% / +0.23% | +0.45% / +0.24% | +0.53% / +0.43% | +0.62% / +0.32% |
| `timers_fire` | -0.05% / -0.13% | +0.05% / +0.05% | -1.25% / -1.23% | -1.26% / -1.24% |
| `timers_due_reversed` | +0.85% / +0.55% | +0.89% / +0.68% | +0.04% / +0.11% | +0.11% / +0.26% |
| `timers_due_stored` | +1.01% / +0.66% | +1.05% / +0.80% | +0.13% / +0.19% | +0.21% / +0.33% |
| `timers_due_join` | +0.29% / -0.70% | +0.47% / -0.46% | +0.51% / -0.54% | +0.59% / -0.36% |
| `timer_await` | +0.90% / +1.75% | +0.82% / +1.91% | | |
| `timer_await_top` | +1.31% / +1.57% | | | |
| `timers_fire_phases` | +0.14% / +0.14% | | | |
| `timers_due_in_promise` | +0.18% / +0.12% | | | |
| `promise_identity_loop` | -0.48% / -0.49% | | | |
| `await_chain`, `all_small_stored`, `fanout_all` | 0.00% | | 0.00% | 0.00% |

- **Every row is within ±3% of the stack's merge base**, on both machines. The tightest is `spawn_many` at T1: +2.88% on x86-64, +2.56% on arm64.
- **`race_yield` against #3256 is +10% to +13%.** The row is new in this stack and measures the stored `yieldNow()` leaf of #3249/#3256, which resumed a stored yield on its poller's next poll: not Node's order. This PR gives stored yields their own turn in creation order, and gives back half of what the leaf gained. Against the runtime's `yieldNow` on main it is 12% to 14% faster, in Node's order.
- **`race_small` is 4% to 5% faster:** `enqueue`'s buffer growth is out of line, so the rest inlines into the race's step again.
- `timers_fire_phases` prints wall-clock times, so its output differs between any two runs.
