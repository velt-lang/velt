# PR 9a working notes (crew #40 follow-on; #3085; #2196 inventory issuecomment-6915776398)

## The #3085 path
- schedSpawn/schedSpawnFut (sched.velt:168-173, 192-197): t.here() 0 → exec() → workers.started() →
  start() on a foreign thread. sched_timers.velt:37,47,56,66 (timerTurn/Again/Clear/Free) call
  workers.started() unconditionally.
- start() (sched_workers.velt:44-75) is thread-affine: t.install(e) (WORKER TLS + EXEC), io.open,
  E_CPU = r.threadClock(), velt_rt_set_scheduler(2) ("the runtime's leaves find its timers and I/O
  from this thread"), startWorkers (lazy worker 0: setLazyWorker TLS in process.velt), end.open.
- After start, schedSpawn from a foreign thread writes worker 0's block non-atomically
  (mk.task, E_PIN, E_LIVE liveUp, sched_make.velt:151-161).
- Thread-safe hand-off that exists: wk.toInbox(h, t) (atomic pushStack on E_INBOX + unpark);
  drained in solo, lazy and shared modes.

## Design sketch (to settle)
1. Mark the program's thread at entry (before module initializers): a hook call from lowering
   (crates/velt_vir/src/lower/entry.rs ~47-49, before initializers) or an OS main-thread test.
   Only that thread may run start().
2. Split start: create() (thread-neutral: allocate the block, vtables, E_MAIN; CAS into EXEC) and
   the thread-affine rest (install, io.open, clock, set_scheduler, startWorkers) run by the
   program's thread on its first exec().
3. Foreign spawn/timers: never install. Spawn: make the task with atomic counts (or hand the
   whole spawn to worker 0 as an inbox item) and push it with toInbox; before the program's
   thread has finished start, the inbox is drained once worker 0 starts. Timers off-worker:
   timerNew already goes to the runtime (bit 0 clear handles); timerTurn/Again/Clear/Free on a
   runtime handle (bit 0 clear) must not call started().
4. Fix schedHandled return (sched.velt:326-328) and schedDetach off-worker (sched.velt:308-309).

## Test (must fail on main first)
New package fixture (like crates/veltc/tests/c_export.rs, needs zig, unix only): lib module
top-level `const early = runEarly();` where runEarly() = C pthread_create + join of an export
that does `spawn(child())` and a `clearTimeout(setTimeout(...))`-style call; child prints.
main: async, awaits, prints. Expect both lines, exit 0, no hang (bounded wait in the test).

## Progress (2026-11-16)
- Branch offworker-9a in velt/branches/offworker-9a (on origin/main 81a2d98b5). Commit cbe0c3741:
  crates/veltc/tests/c_export_start.rs: package `early` whose lib's top-level `runEarly();` (module
  initializer) starts a pthread calling export `early_worker` (prints, `spawn(child())`), main async
  (`await yieldNow(); await sleep(5)`). FAILS on main: exit 101 "there is no reactor running"
  (main's thread off every worker → runtime sleep). Run with
  VELT_ZIG=/Users/andii/.velt/cache/zig/0.17.1/zig-aarch64-macos-0.17.1/zig (else it skips!).
- Decisions:
  1. Program thread mark: new hook (e.g. `sched_enter`/"programThread") in a module importing no
     sched_* (keeps std_scheduler_demand green), called by lowering in build_main
     (crates/velt_vir/src/lower/entry.rs) BEFORE init_natives and init_module_state. Sets a
     ThreadLocal<bool>. Register in std_hooks.rs, std_modules.rs, lazy_std list.
  2. Split start(): create (thread-neutral: alloc block, vtables, E_MAIN; CAS into EXEC, loser
     uses winner's) vs finish (thread-affine: t.install, io.open, E_CPU clock,
     velt_rt_set_scheduler(2), end.open?, startWorkers) run once (CAS) by the marked thread from
     started(). Foreign callers of started()/exec() get the block without installing.
  3. Foreign spawn (schedSpawn/SpawnFut with here()==0): mk.task is fine (alloc + task fields), but
     liveUp (sched_make.velt:147) is a plain store in solo mode → foreign path counts in
     E_LIVE_X atomically and every reader of the live total sums E_LIVE + E_LIVE_X in all modes
     (check sched_end.velt readers); E_PIN not touched off-worker; queue via wk.toInbox(g, k).
  4. Timers: sched_timers.velt:37,47,56,66 must not start on a foreign thread: a runtime handle
     (bit 0 clear) goes to off.*; an executor handle from a foreign thread → ? (rare; document).
  5. schedHandled return (sched.velt:326-328); schedDetach off-worker → runtime path.
- Refined (after reading schedSpawn/end/live): foreign spawn = new function (sched_foreign.velt?):
  block g = EXEC if started, else a PENDING block (thread-neutral init, CAS) that the program
  thread's start() adopts instead of allocating. Task: mk.task(g,...), T_TRANSFER, copy state,
  J_HANDLE, mk.home(g,k); SKIP nc.spawned (worker 0's catch state isn't the C thread's); count
  with m.inc(g + E_LIVE_X) ALWAYS; push m.pushStack(g + E_INBOX, k, T_NEXT) and wk.unpark(g) only
  if started (unpark on an unopened E_IO would break). Readers of the live total must sum
  E_LIVE + E_LIVE_X in every mode: sched.velt:127 solo end loop (use mk.live), sched_make
  liveDown solo branch (add loadRelaxed(E_LIVE_X); measure spawn_many T=1). Runtime waited for its
  off-worker Orphans at exit, so counting them matches condition 3.

## WIP commit (offworker-9a, "wip 9a: program-thread mark, split start, E_LIVE_F counting")
Done: std/lang/hooks/program_thread.velt (+ hook program_thread in std_hooks.rs, std_modules.rs,
lazy_std SCHED_HOOKS + demand; lowering FnLower::mark_program_thread in lower/sched.rs, called in
build_main after register_statics); sched_workers.velt start() split (PENDING block, pending(),
isStarted(), block(), startOn()); sched_task E_LIVE_F=984, E_SIZE=992; sched_queue takeInbox folds
E_LIVE_F (foldForeign); m.add; mk.live sums E_LIVE_F; solo end loop checks E_LIVE_F.
NOT built or tested yet.

TODO next:
- std/lang/hooks/sched_foreign.velt: spawn/spawnFut from a non-worker thread (here()==0 &&
  !isProgramThread): g = workers.pending(); k = mk.task(g,...) BUT T_JOIN must be the SHARED join
  table (worker 0's E_JOIN_VT is unset before start, and is JT_SOLO lock-free in solo mode while
  the foreign thread drops the handle concurrently) → expose mk's shared table; and the join slot
  must lock even under LK_NONE for these tasks (read sched_make.velt lockJoin / joinTable and
  sched_lock.velt kind(): maybe a task flag that forces LK_ON locking, or have the foreign path
  mark the handle detached before returning). Then: copy state, T_TRANSFER, J_HANDLE, skip
  nc.spawned and mk.home; m.inc(g+E_LIVE_F) BEFORE m.pushStack(g+E_INBOX,k,T_NEXT); unpark(g) only
  if workers.isStarted().
- sched.velt schedSpawn/schedSpawnFut: foreign branch first.
- schedHandled return; schedDetach off-worker → off.detach.
- build, run c_export_start (VELT_ZIG!), std_scheduler suites, std_scheduler_demand, goldens; bench
  spawn_many/chan_wake (takeInbox fold adds a load per inbox take).
- Join slot safety for foreign handles (analysis): T_JOIN for foreign tasks = the shared table
  (jn.joinPoll/jn.joinDrop; mk.joinTable shows how tables are named). But even the shared table
  locks via sched_make.velt lockJoin(k, kind) only when (kind & lk.LK_ON) != 0, and in solo mode
  kind is LK_NONE → no lock. Candidate fix: a J_FOREIGN bit in T_JOIN_STATE set at foreign creation;
  lockJoin/unlockJoin take the CAS path whenever the bit is set (one bit test on the plain path;
  keep lk.done(kind)). MUST verify every join-slot writer on the task-completion side (sched_join*,
  sched_run) goes through lockJoin, incl. the solo/lazy tables' fast paths (joinPollSolo,
  joinDropSolo, joinPollLazy) — if not, they need the same bit check. Measure spawn_many T=1.

## WIP 2 (commit "wip 9a: foreign spawn through worker 0's inbox, J_FOREIGN, detach/handled fixes")
Done: sched_foreign.velt (spawn/spawnFut/handOver: shared join table, J_FOREIGN|J_HANDLE,
inc E_LIVE_F before pushStack E_INBOX, unpark if started); J_FOREIGN=64 in sched_task; lockJoin/
unlockJoin take the CAS path when J_FOREIGN; schedSpawn/SpawnFut foreign branch (here()==0 &&
!pt.isProgramThread()); schedHandled returns after off.handled; schedDetach: exec() moved down,
off-worker node → ICE panic (unreachable). c_export_start PASSES now (failed on main).
Next: suites/goldens running; then the coordinator's other tests: (2) block_on on worker 0's
own thread → thrown error (find where schedBlockOn could be re-entered; maybe only a guard + unit
test); (3) a pending off-worker task at exit (foreign spawn that sleeps → program waits; compare
Node: a pending timer keeps Node alive); also "a C callback during main" (after start) test;
bench spawn_many/chan_wake/yield T=1,T=4 (lockJoin bit test, takeInbox E_LIVE_F load).

## Bench round 1 (VM, base 81a2d98b5 vs head wip; results9a.txt, run9a.sh, ~/b9a, ~/tgt-9a-*)
Over 3%: scheduler_spawn_burst T=1 +4.90% (102.0M→107.0M), T=4 +2.47%; spawn_many T=1 +1.38%.
pingpong_colocated T4R0 +3.1% = known scheduling noise. race_small -2.4% (unexplained). libvelt_rt.a
identical size (9a deletes nothing). Per-function (spawn_burst T=1, -g):
 1. schedSpawn no longer inlined into main$poll (head: separate 18.6M fn; main$poll 24.4M→9.2M;
    net +3.4M): the foreign branch (7-arg call) grew it. FIX: move the whole `w === 0` case
    (program thread's first spawn via exec(), and the foreign fg.spawn) into a @noInline helper
    (e.g. spawnOffWorker) so the hot path (w !== 0) is byte-for-byte base. Same for schedSpawnFut.
 2. std_sched_join_poll_solo +1.0M, join_drop_solo +0.6M: the J_FOREIGN test in lockJoin/
    unlockJoin plain path. FIX idea: drop J_FOREIGN from lockJoin/unlockJoin; foreign handles get
    a table that always locks (e.g. jn.joinPollForeign/joinDropForeign = joinPollAs/joinDropAs with
    lk.LK_ON), and the task-completion side (sched_join.velt lockJoin callers ~:25, :160, :216,
    :245 — find where `kind` comes from) uses LK_ON for a foreign task (a T_FLAGS FOREIGN bit,
    tested once per completion, not per join op). Both sides must lock or the J_LOCK CAS vs plain
    store races.

## Fix 1 done (uncommitted at time of writing): schedSpawn's w===0 case → @noInline spawnFirst
(same args, tail call) + @inline spawnOn body. sched.velt now 529 lines (check std-lint).

## Fix 2 — CORRECTED design (current wip is WRONG in solo mode)
sched_run.velt rootDone (~:365): with E_SOLO the completion writes the join slot with PLAIN stores
and never calls lockJoin, so the J_FOREIGN bit in lockJoin doesn't protect solo mode. Do instead:
- REVERT the J_FOREIGN tests in sched_make.velt lockJoin/unlockJoin (back to base: no hot cost).
- Keep J_FOREIGN=64 in T_JOIN_STATE as an immutable marker set at foreign creation.
- Foreign handles: new table in sched_join.velt: @cExport joinPollForeign(f,cx) = joinPollAs(f,cx,
  lk.LK_ON); joinDropForeign(f) = joinDropAs(f, lk.LK_ON); sched_foreign.velt handOver uses them
  (and isJoin() must accept them).
- Completion side, once per task: in rootDone's E_SOLO branch, after `const s = ld32(T_JOIN_STATE)`:
  if (s & J_FOREIGN) → jn.joinDone(e, k, lk.LK_ON); return. In the lazy branch (kind ===
  LK_LAZY): foreign → jn.joinDone(e,k,LK_ON). LK_ON mode already locks (JOIN_DUE → polledFlags; check
  it uses lockJoin with LK_ON). Panic path: sched_join panickedEnd(e,k,kind) callers pass LK_ON for
  foreign. Direct lockJoin callers with lk.kind() (joinHandled ~sched_join:160, joinPanicked ~:245)
  → use LK_ON when (ld32(T_JOIN_STATE) & J_FOREIGN).
- Then rebuild, rerun c_export_start (+ break checks), all std_scheduler*, re-bench spawn_burst/
  spawn_many T=1/T=4 in the VM (~/tgt-9a-head needs a rebuild of head only).
- Fix 2 implemented (uncommitted): lockJoin reverted; kindFor(k) in sched_join; joinPollForeign/joinDropForeign table; rootDone solo+lazy use foreign→LK_ON.

## Review of the #3085 fix (2026-11-16) — MUST FIX before the PR
1. sched_workers.velt start(): the program thread reads PENDING and calls block() without a CAS.
   A C thread's pending() in that window makes a 2nd block whose task is never run. FIX: the
   program thread CASes its fresh block into PENDING (0 → n); if it loses, it uses the winner's.
2. sched.velt is 445 counted lines (std-lint 400, base exactly 400; oversize allow list may not
   grow) → precheck fails. FIX: move schedSpawn/schedSpawnFut (+ spawnFirst, spawnOn) to a new
   std/lang/hooks/sched_spawn.velt and change their hook entries' module in std_hooks.rs
   (sched_spawn, sched_spawn_fut → lang_hooks_sched_spawn; add to std_modules.rs). Check with
   scripts/std-lint (precheck).
3. Crash: schedKeepAliveAcquire/Release (sched.velt) call exec() then end.keepAcquire(t.main()):
   on a foreign thread before start t.main() is 0 → segfault (setTimeout in an export:
   prelude Timer.update → velt_rt_keep_alive_acquire). FIX: use exec()'s returned block. Also
   schedWorkerCount before start → 0 (schedPinned panics), schedUncaughtNow(exec()) on a pending
   block — use the returned block / check.
4. Rescue moves worker 0's inbox (sched_monitor_lazy.velt:176 lazyRescue, sched_monitor.velt:368
   rescueOf) without folding E_LIVE_F → liveDown's "last task" test (sched_run.velt:426
   `liveDown(g,e) === 1 && e !== g`) misses it → worker 0 may park forever in schedFinish; and
   noneLive (sched_make.velt:191) doesn't clear E_LIVE_F. FIX (reviewer's): NO fold. A foreign task
   carries a flag (T_FLAGS FOREIGN or test J_FOREIGN in T_JOIN_STATE) and its end decrements
   E_LIVE_F itself (then unpark worker 0 if mk.live(g) hits 0 and e !== g); noneLive zeroes
   E_LIVE_F; remove foldForeign from takeInbox (also removes its hot-path load).
5. The fallback (MARKED false until velt_main) leaves a C static-constructor thread before
   velt_main able to become worker 0 — document as a limitation in the PR/issue.
   Cosmetic: lower/sched.rs: the new doc comment sits under sched_hook_live's doc — fix order.
Test gaps: also run c_export_start's program at VELT_THREADS=1 and VELT_THREADS=1 VELT_RESCUE=1;
add an export that calls setTimeout (catches 3); ideally one where the C thread is NOT joined
(start a detached thread that spawns while main runs; catches 1/4 in stress).
- fix 1 (CAS in start) and fix 3 (keep-alive uses exec()) applied, uncommitted
- fix 4 applied (no fold; foreign end decrements E_LIVE_F; noneLive zeroes it; m.add removed), uncommitted. Next: fix 2 (move spawn hooks to sched_spawn.velt) + tests (solo/lazy modes, timer in export)
- review fixes 1-4 committed ('wip 9a: review fixes ...'). Next: build, std-lint, tests incl. new test modes
- std-lint fixed (sched_run.velt foreign end moved to fg.ended). c_export_start extended (uncommitted): timer in export + 3 modes (default, T=1, T=1 RESCUE=1). Next: run it; break-check the keep-alive fix with it; re-bench spawn rows; then squash/PR (Closes #3085).
- committed 'wip 9a: spawn hooks re-exported...': demand + bounds suites pass. Next: run extended c_export_start, break-check keep-alive fix, rerun all suites, re-bench, squash, PR.
- coordinator: rebase 9a onto #3090 (awake(e) in sched_wake.velt) once merged; check pending-block adoption vs awake().
- HANG found: unref_std in std_scheduler_workers hung. Cause: task end reads J_FOREIGN from T_JOIN_STATE, which mk.task never initialized (root/unspawned tasks) → spurious foreign path → E_LIVE_F underflow. Fix: mk.task zeroes T_JOIN_STATE.
- REAL hang cause: t.exec() is THIS thread's worker block (WORKER TLS), not worker 0's; keep-alive must use m.ld(exec() + E_MAIN). Fixed in sched.velt keep-alive hooks. (T_JOIN_STATE zeroing in mk.task kept too.)
- committed keep-alive E_MAIN fix; std_scheduler_workers 18/18. Next: full suites, then squash + PR (Closes #3085).
