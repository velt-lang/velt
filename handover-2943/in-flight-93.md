---
name: in-flight-93
description: "Crew #93 (owner 2026-11-18): land #2943 units as PRs. Open: P1 #3196 vir allocs, P2 #3200 blake3, P3 #3203 read-only std (docs-sync site#259). Next: U-series prep"
metadata:
  node_type: memory
  type: project
  originSessionId: 7de780fb-b302-4be3-ab45-b11b64221458
  modified: 2026-11-18T11:22:52.495Z
---

Crew task #93 from win-coordinator (owner decision 2026-11-18): turn branch `units-passes` (worktree branches/d1-open-pr) into reviewable PRs; the lowering cache comes after as its own PR(s) with a design note. Coordinator merges; I never merge ([[coordinator-merges]]).

Opened 2026-11-18 (each with numbers, gate, self-check comment):
- P1 #3196 `vir-allocs`: APPROVED + queued 2026-11-18 (follow-ups: smallvec workspace dep; warm reorder test for prelude_defs). Lowering/verifier allocations; 3-package app build -3.7%. A review found a use-after-free in the new prelude map across KeptLowering::splice: fixed in P1 and on units-passes (4ad75cd92).
- P2 #3200 `blake3-keys` (branches/blake3-keys): codegen_cl cache key + vpm probe hash blake3, docs line; todo_api build -6.2%.
- P3 #3203 `readonly-std` (branches/readonly-std): installed std read-only; docs-sync velt-lang.github.io#259; asked the coordinator to re-run on Windows.

Plan approved: U1 (unit codec) -> U2 (lazy tables + disk; CI runs units-mode goldens from here) -> U3 (builds from the unit) -> U4 (app-stage summaries; includes the per-method "changes this via" mutation evidence for http_handler_changes_through_call: diagnostics must match lazy) -> U5 (units on by default). Each PR at most ~1,500 non-test lines, numbers for hello / todo_api / the 3-package app (tools/pr-ab.sh, scratchpad deps-app), within ±3% of base.
Prep still owed: merge main into units-passes, fix the 7 other units-mode goldens (db_rows waits for #3111), split the 13 oversized files.

**Why:** the owner wants the units win to reach users; U5 turns it on by default.
**How to apply:** one shared target (branches/d1-open-pr/target via CARGO_TARGET_DIR, VELT_STD=$PWD/std per worktree); never build while an A/B or gate uses that target; independent review agent on every diff before the PR ([[self-review-before-pr]]); pre-push tests ([[prepush-identity-safety]]). See [[in-flight-2943]].
