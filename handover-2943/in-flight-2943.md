---
name: in-flight-2943
description: "#2943 compilation units on branch units-passes (crew task #5, mac-7de7): main merged in 6f1abfcc7 (2026-11-18), 8 failing goldens from units; next DHAT in VM, codegen_cl key measure"
metadata:
  node_type: memory
  type: project
  originSessionId: 7de780fb-b302-4be3-ab45-b11b64221458
  modified: 2026-11-18T06:59:36.989Z
---

Branch `units-passes` (WIP, no PR), worktree `branches/d1-open-pr` (own target). Crew task #5 from
win-coordinator; owner's special assignment: build performance only. Full log, decisions and
measurements: `branches/handovers/mac-e688-2943/progress.md` (read its last sections first);
scripts in `.../mac-e688-2943/tools/` (bash/python, run as files).

State at 6f1abfcc7 (2026-11-18, merge of main, pushed; backup ref
backup/units-passes-pre-rebase-1 = ad44ef7de before it):
- Goldens built from std's unit: 8 failing files (val-32 = val-29): union_view x2,
  default_export_of_import, catch_panic, function_call_this, channel_send_copies_resource,
  db_rows_source_throws (waits for #3111), http_handler_changes_through_call. Lazy all pass.
- Instructions before the merge: hello units build ~1.07 G vs lazy 0.42; todo_api units ~6.5 G vs
  lazy ~9.0. Allocation ~30% of a todo_api build (DHAT prof/vm-p3: 6.84 M blocks, 1.1 GB);
  prof/vm-p4 INVALID (old binary vs merged std).
- Done 2026-11-18: DHAT (prof/vm-p5: velt_vir 62% of allocations) -> 3fad11a5a Bits inline,
  e4023275c verifier lists inline, 5e6089ee7/1311407f5 vpm probe hash blake3 (+ docs/internals
  README "Measuring compile time"). Cache keys measured: branch ~1 M vs main's 11.7 M.
- hello from the unit is 3.8x lazy in the VM (665 vs 174 M, prof/vm-p8): decode + its allocs.
  Two "read less" tries reverted (no instr gain). SETTLED: tables are lazy (hello check from
  disk decodes 2.6 k entries); a VELT_UNITS_TOUCH report is only valid with the "loaded from
  disk" line, first count only (it copies everything afterwards). bf129fc1b: std's unit
  stores its functions' final throws (coordinator's design), build_defs skips unchanged std
  fns: rule restored, NO speedup (those defs are decoded anyway). NEXT: same for the 2433
  non-fn rebuilds (finalize.rs:114); the hello build gap (0.84 vs 0.44 G) is spread over
  decode of reached std code + passes walking it: rework passes one by one.
  Wall clock on the busy Mac is useless here; instructions only (tools/fresh-o.sh: hello units
  0.84 vs lazy 0.44 G). Callgrind INCLUSIVE numbers are cycle-inflated: use tools/cg_groups.py (self) or
  VELT_UNITS_ROUNDTRIP=sizes.
- Owed: Windows check that a units build spawning a child reaches exit_win32::exited.
  precheck's file-size rule fails on 13 files (11 already over before; splits with the PRs;
  live.rs units helpers should move to their own file).
- Coordinator: build-perf runs are outside the two heavy slots; still announce heavy start/done.

**Why:** owner wants units (one cached std unit serving every program) as the default; small
programs must not pay for the unit.
**How to apply:** measure every change with tools/ab7.sh against the previous commit, run
goldens from the unit (tools/gold7.sh) before each commit, post MILESTONE notes on crew #5
with counts. Never edit source or rebuild target/ while goldens run. Merge main per
[[rebase-units-passes]]. See [[mac-disk-rule]], [[no-inline-scripts]], [[coordinator-merges]].
