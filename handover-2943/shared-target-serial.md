---
name: shared-target-serial
description: "With one shared CARGO_TARGET_DIR across worktrees, every cargo command is a job on it: never run one from another worktree while a golden suite, gate or A/B uses it"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 7de780fb-b302-4be3-ab45-b11b64221458
  modified: 2026-11-18T11:44:19.594Z
---

When several worktrees share one target directory (the disk rule: one target per session), ANY cargo command from another worktree (a test run, clippy, a build, a gate) rebuilds crates and can replace `target/debug/velt` and `libvelt_rt.a` under a running golden suite, gate or A/B, which then silently measures or tests the wrong compiler.

**Why:** 2026-11-18, #93: while the units-passes merge validation (val-34) ran, I ran the P3 worktree's launcher tests, clippy and a gate into the same target; val-34 had to be treated as invalid and rerun.
**How to apply:** treat the shared target as one job at a time. Before any cargo command, check no background job of mine uses the target; queue the work instead. A/Bs use copied binaries, but still link `VELT_RT_LIB` from the target: same rule. See [[mac-disk-rule]], [[in-flight-93]].
