## Review: changes

Head **6be589fa7** (one commit on f0c0ad596).

**Required 1, x86 `timers_due_reversed`:** taken as measured. A task with `t.ROOT` counts no owner-only promises: `!t.has(task, t.ROOT)` in `pollNode`'s `listing`, in `ownerOnly`'s list and in `leftNested`'s flag. `parks` never reads `main`'s root's count. The golden still prints its output and balances at 1/4/8 workers and with the rescue.

**arm64** (VM, cachegrind, one codegen unit, same compiler, std swapped, base main 4e8bc7f1f):

| row | T1 | T4 |
|---|---|---|
| timers_due_reversed | +1.48% | +1.30% |
| timers_due_stored | +1.18% | +1.03% |
| pingpong_pinned | +1.39% | +1.21% |
| pingpong_colocated | +1.39% | see below |
| race_select | +0.98% | +2.56% |
| spawn_many | +0.59% | +1.49% |
| timer_await | +0.30% | +0.15% |
| due_in_promise, all_small_stored, fanout_all_throwing | under ±0.1% | |
| await_deep, timers | −0.25%, −0.52% | |
| race_small | −3.70% | −3.67% |
| fanout_all | −4.55% | −4.50% |

**`pingpong_colocated` T4:** it's bimodal on **both** sides. Six interleaved runs each:
- base: 1,162.6M ×4 and 1,193.6M ×2;
- head: 1,175.7M ×2 and 1,206.7M ×4.

Mode for mode, the head is +1.12% (low) and +1.10% (high). In these six runs the head landed in the high mode more often than the base. That is the same scheduling bimodality, not a cost of the high mode: base reaches it too.

**Follow-ups:** the sibling case is added to #3323; velt-lang.github.io#283 has the `docs-sync` label; the self-check's item 6 now describes the golden's `sleep(5)`.
