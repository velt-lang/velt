# #3097 step 2: timers (and I/O deadlines) stop pinning a task — design notes

Why anchored today (sched_sleep.velt): worker A's timer block `expire` (:342) moves a due sleep
onto its task's due list (`pushDue` :223, T_TIMERS intrusive circular list, TM_DUE on A's block),
sets the task's flags (`t.flags(k, TIMED|LOCALS)`, "only the thread polling a task changes its
flags"), and wakes it; the task's poll fires them one by one in order with microtasks between
(sched_run fireDue; `fire` :276 bumps TM_UNSEEN on the polling worker's block). All of it assumes
the expiring worker = the polling worker → `sleepPoll` anchors (:129). A shard always has a sleep
pending (its Ticker), so "anchor only while sleeps are pending" would not free it.

Option (a), preferred — move the sleeps with the task:
- Only an OFFER moves an anchored task (worker A gives it to idle worker B; borrow still skips
  anchored tasks, so A's timer block is only touched by A).
- On offering task k: A unlinks k's PENDING sleeps from its own block (scan TM list for
  wk.taskOf(waker) === k, as `forget` does) and pushes them onto B's new atomic stack
  E_SLEEPS_IN (like E_INBOX); B inserts them into its own block at its next look at its timers
  (expire / park). A task with a DUE sleep (on its due list) is not offered this round.
- The task's flags/due list are then only touched by B (its new home) → Node timer order kept.
- Similar for I/O deadlines (sched_io_wait.velt:93-95 anchor): the deadline's sleep moves the same
  way; the socket registration may also need to follow the task (reactor per worker): check.
Option (b), simpler — fire across threads: if k isn't homed on A, `expire` fires the sleep
directly (FIRED + wake the waker) instead of pushDue. Loses "several timers due in one tick
resume in order with microtasks between" for a task with more than one sleep due at once.
Asked the coordinator which (crew #42 note).

## Coordinator chose (a). Implementation plan (worktree velt/branches/timer-unanchor)
- Movability sites (sched_take.velt): `take` (:29, used by takeHalf), `takeFresh` (:128, monitor's
  thread), the input half (:172, `takeHalfInput(w)`, BORROW from another worker's thread).
  OFFER = sched_spread.velt:168 `offer(e, mode, helper)` → `tq.takeHalf(e, false)` on A's own thread.
- Allow ANCHORED tasks only in the offer path: a `t.offerable(k)` = !PINNED && (!ANCHORED ||
  T_TIMERS === 0) (no DUE sleep), passed into take() by a flag; borrow/takeFresh keep t.movable.
- After offer takes its chain: for each ANCHORED task k in it, A scans its own timer block's pending
  list (TM_FIRST… like sched_sleep.velt `forget`) for sleeps with wk.taskOf(S_WAKER) === k, unlinks
  each (unlinkPending) and pushes it onto helper B's new atomic stack (new E_SLEEPS_IN in
  sched_task.velt, link S_NEXT; m.pushStack). Keep ANCHORED (its sleeps now live on B).
- B adopts at every timer look (sched_park expire path, sched_tick/expireOwn, sched_order step if it
  expires worker blocks): takeStack(E_SLEEPS_IN), insert each into block(B) (sched_sleep.velt
  `insert`, keeps (deadline, seq) order; block(e) opens the block lazily). Offer already unparks B,
  so B recomputes its park deadline after adopting.
- Same-thread invariant: A's block touched only by A (offer runs on A), B's only by B. No race with
  A's expire (same thread). DUE sleeps never move (task not offered that round).
- Tests: Node-checked golden: a task with 2-3 sleeps due in one tick (Promise.all of sleeps with
  the same deadline, plus microtasks) that is moved between workers (force: VELT_THREADS=4 + busy
  worker 0 so it offers); stress test (Rust): many tasks sleeping short random intervals while
  worker 0 is kept busy so it offers them, repeated; check every sleep fired exactly once (counts)
  and VELT_RT_DEBUG_ALLOC/VELT_RC_STATS show no leaks.
- I/O deadlines (sched_io_wait.velt:93-95): investigate whether the socket's reactor registration
  (per worker, fd_socket.velt:105) can move cheaply; if not, report on #3097 before building.

## REVISED (offer has no fixed destination)
`offer` (sched_spread.velt:168) pushes taken tasks onto the OFFERING worker's own `E_OFFER` stack
and only wakes `helper`; ANY worker out of work `steal`s them (sched_spread.velt `steal`, j===0 is
the worker's own offer first, and the stolen tasks' home becomes the stealer). So the sleeps can't
be pushed to B at offer time. Instead the task CARRIES them:
- offer (A's thread): for an ANCHORED task with T_TIMERS === 0 (no due sleep), unlink its pending
  sleeps from A's block (scan as `forget`), link them into a list kept in the task's T_TIMERS (free
  since no due sleeps), set a new task flag CARRIED (sched_task.velt flags; pick a free bit).
- steal (stealer's thread, before the task can run): for each stolen task with CARRIED: take the
  list from T_TIMERS, set T_TIMERS 0, clear CARRIED, insert each sleep into block(stealer) with
  sched_sleep.velt `insert` (opens the block lazily); keep ANCHORED (now anchored to the stealer).
- Every block still touched only by its owner. No new executor field. Offer's take must allow
  ANCHORED-with-no-due tasks only on this path (takeHalf(e, false) is called from offer AND
  maybe elsewhere: check callers; add a parameter or a separate takeHalfOffer).
- Possible snag: sched_sleep's TM_* bookkeeping (TM_SEQ numbering per block — a carried sleep keeps
  its S_SEQ from A; ordering (deadline, seq) across blocks only matters within one block: OK but
  verify Node order for equal deadlines from different blocks isn't assumed).
