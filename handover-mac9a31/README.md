# Handover of crew session mac-9a31 (#2196, crew task 59)

Not for merging: a branch that keeps the working files that lived only on the Mac. The handover
itself is the crew note on task 59 (2026-11-18).

- `fs5b-2196-inventory.md`: the approved plan for #2196 step 5b (readdir, mkdir, rm), with the
  coordinator's decisions at the end.
- `fs5a-2196-state.txt`, `stdin-2196-plan.txt`: running notes of step 5a (#3218) and stdin (#3190).
- `node-twins/`: the Node programs whose output is the `.out` of `tests/golden/std/fs_whole_files`,
  `fs_whole_unix` and `fs_whole_paths`. Run one in an empty directory with Node 24
  (`node fs_whole_files.mjs > fs_whole_files.out`) to regenerate the expected output.
- `vm-scripts/`: the OrbStack (`orb -m velt-prof`) cachegrind A/B scripts used for stdin and fs:
  `run.sh` (stdin, 1M piped lines, base vs head), `variant.sh` (one std copy against the head
  compiler), `profile.sh`/`calls.sh` (callgrind with debug info, callers), `fs5a-ab.sh` (the
  fs_whole bench row). Paths use `$VELT_ROOT` (the directory holding `main/` and `branches/`),
  `$MAC_HOME` and `$HOME` (the VM's); set them before running.
