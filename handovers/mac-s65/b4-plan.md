# B4: the tokio host leaves velt_rt (plan, mac-s65, 2026-11-27)

Files: crates/velt_rt/src/task/runtime/{mod,host,handle,time}.rs, task/quiet.rs, task/leaf.rs
(External count), entry.rs (wait_for_the_end, tasks_may_run), lib.rs/debug_alloc.rs comments.

Order: after A3 (net-rt-streams-go) lands: net/stream.rs, later.rs, views.rs are the last
users of runtime::spawn and runtime::time. After A3, net/tcp.rs keeps only velt_rt_tcp_listen
(a leaf around dev::bind, plain since C3 #3911) and net/sys.rs is dead (aio went with #3609):
delete sys.rs in A3 or B4.

What still reaches the host on main (besides net):
- velt_rt_set_scheduler(2) (std/lang/hooks/sched_workers.velt): enters the host's tokio context
  on every worker, so leaves find tokio timers/I/O. With no tokio leaves left, it becomes a no-op;
  keep the export (old programs, velt_vir Rt::SetScheduler) as an empty function.
- velt_rt_keep_alive_acquire/_release: std hooks replace them under the std scheduler
  (std_hooks/scheduler.rs lists them; velt_vir lower/sched.rs maps them). Check whether anything
  native still calls the Rust ones; if not: DELETED list (velt_common std_hooks/deleted.rs).
- velt_rt_timer_unref_due (sched_end.velt probes it as `velt_rt_timer_unref_due$probe`):
  quiet.rs UnrefDue, built on the host's park hooks and a tokio Sleep. Without the host it has
  no workers to wait for: the answer is "ending" iff keep_alive == 0 && no pending leaf && no
  queued task, which std knows itself (sched_end.velt). Move the probe's logic into
  sched_end.velt (a std hook; hook_replaces names the extern) and delete UnrefDue.
- entry.rs: wait_for_the_end() and tasks_may_run(): with no host tasks, alive_tasks() == 0;
  wait_for_the_end only waits on Rust KEEP_ALIVE (who takes it natively? check) -> likely
  delete both and quiet::wait.
- leaf.rs External (external_begin/_end): counts pending Rust leaves for quiet.rs. Remaining
  leaves after A3: velt_rt_tcp_listen (dev), registry closed_leaf (ready at once), latch.rs Wait.
  If std's end check already counts its own pending futures, External goes with quiet.rs.
- velt_rt_worker_count / velt_rt_current_worker: old-ABI answers, no tokio; keep.

Tests to move/delete: runtime/mod.rs tests (keep-alive publish, idle publish), quiet.rs tests,
tests/abi fake driver (B3 took part of it), task/local/* (B's; check what is left).

Bar: goldens + std tests debug/release, scheduler tests at 1/4/8 workers (VELT_THREADS),
exit-order goldens (unref'd timers: tests/golden/*unref*, *exit*), serve_cost/fetch/async rows
and compile rows by instructions on the builder (ask the coordinator: a Windows session runs them).
Then C5: tokio, futures-util, bytes, socket2 (dev/mod.rs no longer uses it? check) out of
crates/velt_rt/Cargo.toml; velt_native's "tokio" dev feature; tests/abi fake.rs driver.
