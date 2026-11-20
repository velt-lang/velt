HANDOFF mac-7614 (crew #125, 2026-11-20)

OWNED, OPEN
- PR #3261 (#2196 9c PR B: prebuilt module_blocks). Branch module-blocks-prebuilt @ 5a77956b5 (pushed; nothing local). APPROVED (round 3). Already contains main as of train 12d (merge 5a77956b5: no conflicts, PR diff unchanged, checks green). Coordinator: it goes AFTER the 9d stack (#3249/#3250/#3256, train 15).
  Left, in order, once the 9d stack has landed:
  1. In worktree /Users/andii/dev/velt/branches/module-blocks-sync: git fetch; git merge origin/main.
  2. Expected textual conflict: crates/velt_vir/src/lower/sched.rs. Keep the stack's sched_yield_ready and drop sched_module_exit (this PR removes that interim hook; moduleExit replaces it).
  3. Auto-merged files to check against the PR's own diff: std_hooks.rs, std_modules.rs, runtime_exports.txt, velt_rt task/local/mod.rs, lazy_std live.rs and mod.rs, rt_abi.md, sched.velt, sched_exit.velt. Quick check: `git diff --numstat <merge>^2 <merge>` (old main side) vs `git diff --numstat origin/main HEAD` should match except where 9d itself overlaps.
  4. crates/velt_common/src/std_hooks.rs must stay <= 800 lines (798 now; module-state rows live in std_hooks/module_hooks.rs).
  5. #3261 adds no HIR enum variant, so the units-codec rule from train 14/15 doesn't apply (recheck after the merge).
  6. Ask `heavy?`, then run notes/mac-7614/check3261_main.sh from branch wip/mac-7614/notes (fresh build; the worktree's target was removed): cargo build --workspace; VELT_GOLDEN=module_ goldens; veltc tests prebuilt_module_blocks, prebuilt_weak, prebuilt_hooks, module_state_attrs, module_state_races, opt_hot_loops_vir; velt_opt module_state; velt_common; veltc --lib prebuilt; cargo xtask precheck.
  7. Push, comment on #3261 what was merged and run, and give the coordinator the sha for a train.
  Interaction with merged #3300: on arm64 Linux, debug builds take no prebuilt objects (prebuilt.rs debug_link_takes), so module_blocks is compiled in there. Nothing to do for that.

MERGED TODAY (no action)
- #3300 (closes #3262: the toolchain ships prebuilt hook objects in lib/hooks/<target>, manifest form 2 with object hash + end marker, clang-18 in release.yml's Debian jobs) and #3313 (closes #3312: clean-machine smoke builds scripts/smoke-prebuilt.velt and requires 'object found (shipped)'; on arm64 Linux it only requires the program to build and run). Train 14.
- Earlier: #3206, #3267 (crew #129, done), #3270/#3271/#3278 (crew #130, done).

OPEN ISSUES I FILED (unclaimed)
- #3315: debug links on arm64 Linux lack the __aarch64_* outline-atomics helpers that LLVM objects call (shared runtime doesn't export them; bundled kit has no libgcc.a). Proposed fix: velt_link puts the static runtime archive AFTER the shared runtime on outline-atomics targets (never among the objects: it would pull runtime members). Then drop debug_link_takes and the arm64 exception in scripts/smoke-clean.sh. Same latent bug for the std cache's debug objects in bundled-linker debug builds.
- Docs: velt-lang/velt-lang.github.io#282 (shipped hook objects, stats line, arm64 debug exception): a release comments its tag there.
- Not filed (mentioned in #3300's review follow-ups): source builds without a git hash share a version line, so their shipped objects could be taken by each other; a build id in the manifest would close it.

STATE
- Backup branch: wip/mac-7614/notes @ 5aff5f55b (notes/9c, notes/130 VM scripts; notes/mac-7614/ check scripts). Not for main.
- Nothing running, nothing on the VM, no measurements in progress. Worktree module-blocks-sync kept (no target dir); ship-hooks/shipped-check-3312 worktrees removed.

TIPS
- crew watch truncates long messages: ask the sender to resend the tail.
- Release dry runs (release.yml workflow_dispatch) are dispatched by the coordinator only; signing-key fails on branches, which is expected.
- Never put the static runtime archive among a link's objects (see #3315).
