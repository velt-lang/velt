# crew #40: Promise.race in std (#2196 step 2) — working notes (not committed)

Task (win-coordinator): Promise.race/any through a std hook (schedRace, velt_rt_race's ABI), Node
order (first settled wins; losers' later settlements ignored, no leaks; a losing rejection not
reported unhandled when Node wouldn't). Goldens vs Node: mixed arrays, race vs never-settling
child, empty array, #1362's race case. Bench within 3% (race, select-style, spawn_many). One PR,
"Part of #2196" and "Part of #1362", built on #3069's branch (std-promise-all), rebased later.
Deleting velt_rt_all/velt_rt_race: coordinator's fallback applies (see below).

Worktree: velt/branches/std-promise-all, branch std-promise-race (on std-promise-all-main 851a61389).

## Research (2026-11-16, Explore agent)
- Off-worker = code the runtime's executor runs on its own threads (C callbacks, runtime leaf
  threads), NOT pinned spawns (sched_off.velt header). std objects aren't driven there.
  => keep velt_rt_all*/velt_rt_race* for sched_off.velt only; comment naming it their last user;
  one line on #2196 (they go when the off-worker path moves, tokio plan comment 6895982860).
  Also velt_rt's Child/give_up_in_order/ResultDropFn are used across task/* and ABI tests.
- wasm: velt_rt_wasm has its own race.rs/all.rs (std_sched() false for wasm) — untouched.
- Lowering: async_fn/tasks.rs promise_race (~188-214): Rt::Race(futs,n,size) or
  Rt::RaceOk(futs,n,size,reject_drop|0) (RaceOk only when elements can reject), preceded by
  Rt::FutsHandled. The race future is returned UNBOXED (owned_result; slot at +16).
  Rt::Race/RaceOk not in sched_rt's list (sched.rs ~206). Other user: I::PromiseAny via
  std firstFulfilled (std/prelude/promise.velt:58).
- Because the race is unboxed, std hooks that get it as `f` would send it to the runtime:
  schedTransfer (sched_node_transfer.velt:32-39 → off.transfer → pass_transfer), schedPeek,
  schedShare (off.share), schedDetach, schedHandled. Options: (a) box the race in lowering like
  all (FutBox → node; one alloc more per race: bench risk), or (b) add a race case to each hook.
- Node-order inputs in std:
  - started node: m.ld(f) === fnAddress(nd.nodePoll); n = nd.nodeOf(f) (f - N_HEAD 128);
    flags m.load32(n + nd.N_FLAGS): STARTED=1 FINISHED=2 ... (sched_node.velt:44-87).
  - shared holder: m.ld(f) === fnAddress(sh.holderPoll); hub m.ld(f-8) unless
    sh.forwardOf(f) !== 0; settled: m.load32(h + H_FLAGS(40)) & DONE(1) (or H_NODE FINISHED).
    NO settlement order exists (runtime has settled_at): add H_SEQ to the hub, set in
    sched_share_settle.velt `settle` from a global counter, for settled_holder.
  - converted: node test passes and al.holder(nd.nodeOf(f)) (sched_share_alias.velt:78) != 0.
- Rust race semantics (race.rs): first poll in array order skipping Turn::Pending children
  (started-not-done, shared-not-settled, converted-of-unsettled); settled holder with the
  earliest settlement wins over wake order; race_ok drops rejections unless last; winner's
  result → race slot; losers given up in array order; drop: children dropped in array order;
  n == 0 never completes.
