Self-check (CONTRIBUTING "Before opening a pull request"):

1. **Node**: the golden uses Velt's `spawn`, `sleep` and `yieldNow`, so it isn't plain TypeScript and can't run under Node. Where Velt differs on purpose (the dispose hooks of what a freed task held run; Node collects the same functions and runs nothing), the golden's header says so.
2. **Docs**: user-visible: such a task is freed and its values' `[Symbol.dispose]` hooks run. Docs-sync issue: velt-lang/velt-lang.github.io#283. The design is described in sched_task_lost.velt's module docs, which this PR adds.
3. **Tests**: golden `tests/golden/lang/task_parked_freed` (`check: no leaks`). Break checks, each failing the golden at 1/4/8 workers and with the rescue:
   - the park check off (`runEnd` never finds a parked task): 858 blocks left, no `freed` lines;
   - the nested exit off: 49 blocks left (the chains started before they were awaited);
   - the handle-drop wake (`J_PARKED`) off: 22 blocks left, `freed late handle` missing;
   - a listed promise given a waker after listing ("a run hands out the waker"): `ICE: an owner-only promise gained a waker` at the park walk, every run. Given before listing, the link's check catches it.
   On main the golden leaks 809 blocks.
4. **No unwrap on user input**: std only, no Rust changed besides the wasm golden list.
5. **Benchmarks**: arm64 VM, cachegrind, one codegen unit, same compiler, std swapped, base main 9cb569834, T1 (T4 runs of the pingpong and race rows are bimodal run to run, in base and head alike):

   | row | T1 |
   |---|---|
   | timers_due_reversed | +2.85% |
   | timers_due_stored | +2.68% |
   | pingpong_pinned / colocated | +1.60% / +1.67% |
   | race_select | +1.16% |
   | spawn_many, timer_await, await_deep, fanout_all_throwing | +0.6% to +0.8% |
   | due_in_promise, all_small_stored, timers, fire rows | under ±0.5% |
   | race_small | −3.6% |
   | fanout_all | −5.6% |

   The first version (a linked list per task) was +6.6% on the due rows; the count with the flag set by the adoption's own read-modify-write brought them under 3%. The two rows that went down: fewer blocks live (parked promises freed). x86 rows: the coordinator measures them on the builder.
6. **Sleeps**: the golden waits on the dispose hooks (with a timer fallback for Node), and its late-handle case has one `await sleep(5)`, marked `// timing:`. That case's output doesn't depend on it (a drop that comes first gives the same lines), and at one worker the `J_PARKED` path runs deterministically.
7. **Size**: std files within 400 lines besides doc comments (std-lint clean).

**Gate**: on main 9cb569834 with this std, through a built main compiler: scheduler suites + c_export_async + goldens 252/252, std's own tests 624/624, golden at every mode. Then merged with main 4e8bc7f1f (train 14, clean merge) and squashed. The push hook's std-lint step couldn't run in this worktree (no runtime library in its target), so I ran std-lint by hand with the same arguments: clean; fmt and clippy passed in the hook.

Follow-up for the case this leaves (a suspended promise handed to a task on another worker and dropped there): #3323.
