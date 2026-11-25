# mac-6dbf handover (#3739 scheduler order chain), 2026-11-25

Scripts and probes behind #3739 round 1 (PR head 57ed94d26). The scripts hard-code the old
session's scratchpad (`S=...`), the worktree `branches/root-turns-3687-r1` or `root-turns-final`,
and the target `branches/mac6dbf-target`: set those first. Set `VELT_RT_LIB` explicitly when one
target serves several worktrees (else `velt test` may link a stale runtime from
`<target>/*/targets`).

- `s3739f.sh`: the gate (build, 3 goldens x 4 modes, token/held tests, 12 golden sets, std tests,
  eager_std_identity, std_lint, velt_sema/velt_vir, precheck).
- `check_merge.sh`: goldens, tests, probes vs Node, arm64 rows (PR1 / an older std / this std).
- `brk_merge.sh`, `brk3687r.sh`: break checks (std edited in a copy); `brk_targets.sh` dry-runs
  their swap targets.
- `rows_iay.sh`, `race9.sh`: arm64 instruction rows (iay2 = rows/iay2.velt, the coordinator's x86
  program).
- `probes/`: merge_edges, listed_after, listed_root (Node twins via `probes/node_run.sh`; the one
  known diff in merge_edges is #3689's await-step order).
- `exception3739.md`, `reply3739.md`, `pr3739_final.md`: what was posted on #3739.
