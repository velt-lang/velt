Found in the #3583 review (std scheduler, not caused by #3583).

**The shape:**
- Task X has a promise; a holder of it, `h.p`, is handed to task Y and shared there.
- Task Y awaits a race over `[q, h.p]`, where `q` is another of X's promises that finishes after the shared one.
- Task Y also awaits `Promise.all([h.p])`.

**What happens:** at 4 workers with a 30 s timeout, the program hangs:

| std | runs that hung |
|---|---|
| main's | 2 of 5 |
| #3583's | 1 of 5 |

The reviewer's program, `away_watch2.velt`, is to be attached here.

**Where to look first:** the away hub's watch of the original promise (sched_share_away.velt `watch`, from sched_share_drop.velt `release` when its first waiter leaves), and a `Promise.all` holder waiting behind a racing one.
