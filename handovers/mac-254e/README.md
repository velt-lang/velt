# Handover: mac-254e → mac-7897 (2026-11-24)

Crew task #162 (scheduler: #3487/#3498), plus #239 (#3690), #244 (#3731), done #238/#240 (#3588 arm64).
Never merge: the coordinator (win-coordinator) merges. Paths here use `$HOME` and `$SCRATCH` (a scratch dir).

## Open items

### PR #3583: std: a promise that finished lets go of its task (#3498)
- **Branch** `let-go-3498`, head **eb9c6e769** (main merged in at 1dbbbb6ab, on top of 9755cd194).
- **Review:** approved in round 3 (share_orig_handed bites; rows within ±1.5%).
- **Train:** failed on Windows: `ICE: std without the hook sched_all::schedAll`, in the new `std/lang/hooks/sched_node_done.test.velt`, release.
  - The fix, eb9c6e769: the test imports `velt/lang/hooks/...` instead of `./sched_*`.
  - The coordinator confirmed it on Windows 11: debug and release pass with a fresh VELT_HOME. The approval stands.
  - It goes into the next train.
- **Next step:** watch the train. If a train needs main merged in, the coordinator does it; don't push merges to a branch that's in a train.
- **Then (follow-up PR, from main after #3583 lands):** the counted ICE plus the pause knobs.
  - **The counted ICE:** branch `wip/mac-254e/let-go-stack` commit e6263e4cc. In `sched_node_wake.velt` `wakeAway`: `if (wk.counting() && m.ld(n + nd.N_TASK) !== task) panic("ICE: a promise let go of its task while a wake of it by value was under way")`. Plus `wp.beforeTaskRef()`, a pause in sched_wake_pause.velt.
  - **The pause knobs** the reviewer asked for, so that item 1's guards (`handed_all`, `away_first_leaves` in std_scheduler_held.rs) fail with the fixes reverted:
    - one between `pollNode`'s flags load and `awaitWith`'s task load;
    - one between a finish and a holder's drop.
  - **Done means:** each guard fails with its fix reverted and passes on head; rows within ±3% on x86 (cachegrind, `perf/vm.sh`) and arm64 (`perf/mac.sh`). The knobs exist with the counters on only (`VELT_SCHED_WAKE_PAUSE`-style, see sched_wake_pause.velt).
- **Follow-ups filed, not claimed:**
  - #3595: test `WATCHED` instead of `REJECTING` in letGo, so that fulfilled, can-reject promises let go;
  - #3593: the away_watch2 hang, race + `Promise.all` over a promise handed to another task and shared there (also on main).

### PR #3729: veltc tests: the two flaky scheduler tests order their threads by atomics (#3690)
- **Branch** `flaky-3690`, head **b0833a37f**, base main. Crew task #239.
- **Review:** approved by the coordinator.
- **Next step:** #3722 (macOS skips of both tests, in train-1124-1438) must land first. When it shows MERGED:
  1. merge main into flaky-3690;
  2. remove #3722's two `#[cfg_attr(target_os = "macos", ignore = "flaky on loaded macOS runners: #3690")]` lines (std_scheduler_timers.rs `a_pinned_spawn_runs_its_timers`, std_scheduler_wake_order.rs `a_carried_wake_still_queued_when_main_ends_is_given_back`);
  3. run precheck, then push.

  The coordinator said: "no need to run the full gate locally, the train runs it. I'll train it then."
- **Evidence** (in the PR): exit_queued was 55/200 bad at T4 and 22/200 at T8, and 0/300 each after. A 500 ms stall in the pinned task reproduces the CI timers failure 20/20 on the old program and 0/20 on the new. Scripts: `t239-flaky/` (loop.sh, hog.sh, late.sh, the programs).

### PR #3742: loader: one std file gets one module name, however the path that reached it is spelled (#3731)
- **Branch** `std-key-3731`, head **de7a9191a**, base main. Crew task #244.
- **The fix:** velt_loader `Origin::canonical` (locate.rs `relative_within`) retries with `file_key` paths when the plain relative path leaves the root.
- **Tests:** velt_loader `spelling_tests.rs`:
  - a Unix link both ways (fails with the retry removed: names `std/../std/x/...`);
  - a Windows verbatim test file with a plain std root, and the reverse.
- **Gate** at c5ba4d044 on macOS: 2765/2766. The failure is `cpu_levels::half_dot_products_on_this_cpu_print_the_golden`: Homebrew clang can't select `bfdot` with `-mcpu=native`, unrelated.
- **Windows:** the coordinator ran c5ba4d044 there. Spelling 2/2. The break check showed main's loader ICEs only with mixed spellings, and the fix passes them.
- **Next step:** de7a9191a added the reverse Windows case (cfg(windows), never compiled here) and dropped the std test that couldn't bite. Ask the coordinator to run `cargo test -p velt_loader spelling` on Windows at de7a9191a, then wait for review.
- **Open question asked on the PR:** should sema's `sched_hooks` (lazy_std/mod.rs) report a demanded hook it can't find, instead of `filter_map` dropping it silently? It would be a separate velt_sema change; waiting for an answer.

## Done this session (for reference)
- **#3559** (wake-order) merged.
- **#238/#240:** #3588 arm64 counts. The final at 37ce5916b against main 70bb0f818: every row at or below main (ECDSA verify −10.5%, ECDH −8.5%, verified handshake −3.5%, X25519 +0.2%). Programs and job scripts are in `t238-3588-arm64/`.
- **#3731** filed and fixed (above).

## Local-only knowledge and traps
- **Lazy std and relative imports:** std test files must import hook modules by `velt/lang/hooks/...`; a relative `./` import broke Windows trains (#3731) until #3742 lands.
- **The macOS SDK:** release `velt` builds on this Mac need `SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk`, and `VELT_RT_LIB=<target>/release/libvelt_rt.a`.
- **Timeouts:** macOS has no `timeout`; use `perl -e 'alarm shift; exec @ARGV' 60 <cmd> > out 2>&1`.
- **Instruction counts:** `/usr/bin/time -l` gives instructions retired. Use VELT_THREADS=1, min or median of 5, interleaved.
- **x86 rows:** use cachegrind in the OrbStack VM `velt-prof` (`perf/vm.sh build|vars|reps|prof|clean`). Scheduler costs show on x86 that macOS hid.
- **The flaky pinned-timers test** didn't reproduce in 1800 runs even with 20 `yes` hogs. Only a simulated stall reproduces it.

## House rules
- **Heavy jobs:**
  - ask "heavy?" on crew and wait for a go that names you;
  - post `crew send all "heavy start ..."` and `"heavy done ..."` (there's no `crew msg`, and never hide crew's output);
  - one job per slot; `CARGO_BUILD_JOBS=3`, `CARGO_INCREMENTAL=0`.
- **Disk:** above 30 GiB free before any heavy job. Delete targets and their `debug/incremental` and `release/incremental` dirs when done.
- **Probes:** every probe and break check gets a timeout and writes to a file, never an unbounded pipe (a `diff -` pipe filled the disk once).
- **The gate:** never edit a tree while its gate runs.
- **Before every push:** precheck, `eager_std_identity` and the std safety-comment test.
- **Git:**
  - never push merges of main to a branch that's in a train;
  - squash onto the merge base, not origin/main;
  - commits are authored `1434571+andtii@users.noreply.github.com`, with no co-author trailers or "Generated with" footers.
- **The bar:** Node-identical output; rows within ±3% by instructions on x86 and arm64; a new test must fail on a broken version.

## Build state
- **Deleted:** every target of mine; the worktrees fix-3420-254e, flaky-3690 and std-key-3731 go after this push. Rebuild whatever you need: a debug velt is about 3 GiB and 15 min here.
- **Branches kept on origin:**
  - let-go-3498, flaky-3690 and std-key-3731;
  - wip/mac-254e/let-go-stack (#3559 + #3583 + the counted-ICE WIP, c3bdcbfb3);
  - wip/mac-254e/held-done-3498 (design B of #3498, f0222a8a5, superseded by A'; delete after #3583 lands);
  - handover-mac254e (this directory).
