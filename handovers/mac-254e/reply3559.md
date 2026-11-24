Review addressed in 505ffe126.

**The pause** (`VELT_SCHED_WAKE_PAUSE`) now sits in `wake` itself (sched_wake.velt), after a successful `IDLE` → `SCHEDULED` and after a successful `RUNNING` → `NOTIFIED`. Every wake by value passes one of those, in either order. The carry path keeps its own pause after its `schedule`, and `locked` drops its explicit one, since it calls `wake`.

**Plain reverts, with no pause added by hand:**

| Revert | Result |
|---|---|
| `wakeGiving` (task waker) back to wake-then-release | 2 of 6 wake-order tests fail (`awaited_dropped`, `stolen_carry`) |
| the node waker's `wakeGiving` back to the old order | 1 of 6 fails (`node_woken_away`) |

**Checks on 4f0ae73e0:**
- the gate: 5668 of 5669 (#3440);
- wake-order 6/6, bounds 7/7, precheck.

**Rows:** the pause test adds one relaxed load per wake. Within ±3% on x86 (cachegrind) and arm64.

| Row | x86 T1 | x86 T4 | arm64 T1 | arm64 T4 |
|---|---:|---:|---:|---:|
| `chan_wake` | +1.80% | +0.64% | +1.65% | +0.22% |
| `timers_fire` | +0.43% | +2.67% | +0.33% | +2.11% |
| `race_select` | +0.48% | +2.28% | +0.55% | +2.56% |

- **`race_select` on x86 at 4 workers** is the median of 5 interleaved runs. Single runs there are bimodal: a minimum of about 245M on both sides, a median of about 523M.
- **Everything else** is within ±1%, apart from `timer_await` arm64 T4 at −6.7%, an improvement.
