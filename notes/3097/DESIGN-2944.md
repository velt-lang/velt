# std Promise.all join (#2944, crew #8 B) — working notes (not committed)

Plan: #2196 comment 6892757897. Branch std-promise-all on #3008's branch (std-promise-watch,
b15c78371); rebase onto main once #3008 merges.

## Lowering
- `Rt::All | AllWithDrop | AllOrReject` (crates/velt_vir/src/lower/async_fn/tasks.rs:118-134)
  divert in `sched_rt` (crates/velt_vir/src/lower/sched.rs, list ~209) to hook `sched_all`
  `schedAll(futs: usize, n: u64, size: u64, results: usize, rdrop: usize, reject: bool) -> usize`
  (rdrop 0 for Rt::All). Register like `sched_sleep`: std_hooks.rs, std_modules.rs, lazy_std.
- The returned heap future is wrapped by lowering (Work::AllPoll / settle) and FutBox'd, which
  under std is a node: the join settles through that node (microtask), no extra work.
- The caller frees the pointer array right after the call: copy the child pointers.

## Join J (one m.alloc: header + n child records)
- +0 poll, +8 drop, +16.. fields (unit result, nothing read):
  J_N, J_SIZE, J_RESULTS, J_RDROP, J_REJECT, J_LEFT, J_STARTED (first pass done),
  J_READY (atomic stack of child records, link C_NEXT), J_WAKER (awaiter, under J_LOCK),
  J_LOCK (u32, lk.kind()), J_REFS (atomic: J itself + every clone of a child waker),
  J_STATE (0 running, 1 complete, 2 dropped), J_CX (2 words: scratch VeltCx for child polls:
  +0 = the awaiter's await-word pointer (cx+0), +8 = the child's waker record).
- Child record C_i (48 bytes): +0 table (main()+E_ALL_VT), +8 join, +16 fut (0 once finished or
  given up), +24 next (ready link), +32 queued (atomic 0/1), +40 done (result in slot, for rdrop).
- Waker table E_ALL_VT in worker 0's block (like E_NODE_VT, set in sched_workers.velt):
  clone: J_REFS++ ; wake = wake_by_ref + drop ; wake_by_ref: cas queued 0->1 then pushStack(J_READY)
  and wake the awaiter (clone J_WAKER under the lock, wakeOwned after unlock) ; drop: J_REFS--,
  free J at 0.
- poll(J, cx): register awaiter waker (lock; clone if changed). First poll: children in array
  order (#2857: an input already settled settles on this pass). Then: take J_READY, reverse
  (wake order), clear queued before polling each, poll only those. Child ready: copy `size`
  bytes from fut+16 to results+i*size, futDrop, done=1, LEFT--. REJECT && tag byte != 0: move
  slot i to slot 0 (rdrop slot 0 first if done), give up the other pending children in array
  order (schedDetach-like give-up; they were marked handled by FutsHandled), complete.
  LEFT == 0 (or n == 0): complete, return 1.
- drop(J): if not complete: futDrop pending children (array order), rdrop done slots; then
  J_STATE = dropped, drop J_WAKER, J_REFS-- (free at 0).

## Tests / numbers (plan)
- Goldens (Node-compared): mixed array; rejection while pending (both orders); empty array;
  #2857's two cases; #1362's all case. std unit tests: wake order, give-up order.
- Existing combinator goldens. Bench: every row ±3%, spawn_many T=1, T=4, T=4 RESCUE=0;
  build time + IR size. Close #2944 with numbers; "Part of #1362". Nothing added to velt_rt.
