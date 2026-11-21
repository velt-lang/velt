# Handoff: mac-4d96 (crew task #135), 2026-11-21

Crew: hub http://192.168.115.247:4747, coordinator `win-coordinator`. I took #135 over from mac-60d0
(its handoff: `notes/mac-60d0/HANDOFF.md` on `origin/wip/mac-60d0/tools`). Rules as on the crew:
heavy work only on a "go now" that names you; post `heavy start` / `heavy done`;
`CARGO_BUILD_JOBS=3`; Mac disk >= 30 GiB free; ±3% per bench row on x86 and arm64.

## Open PRs

| PR | branch | head | state |
|---|---|---|---|
| **#3420** (closes #3323: a promise dropped or freed on another worker than its task's is given to that task) | `parked-away` | 661b95982 | Round 2 pushed and answered on the PR item by item (CHANGES NEEDED round 1: items 1-3). **Waiting:** the arm64 rows were queued in slot B after mac-77e0 and NOT run. Re-review after them. |
| **#3273** (#3207, sleep hand-over; includes the owner-word ABA fix) | `sleep-home` | dd51c27a9 | ABA fix accepted (720/720). Sole blocker R1: `spawn_many` T4 on x86 +4.5%. arm64 12-run interleave shows -0.46% / +0.38% (posted). **Waiting:** owner ruling as noise; the coordinator asked the Linux session for an x86 A/A. Nothing to do unless the ruling says cut. |
| **#3369** (part of #3360: a promise settled from another task lets go of the value before it opens the latch) | `settle-race` | d8d28f12b | Review items answered: bare promise filed as #3389 (fixed by mac-77e0's #3411, stacked on settle-race), resolve off a task filed as #3390 (mac-166e, stacked on settle-race). **Waiting:** re-review. |

Merged: #3322 (#3274) on 2026-11-21.

## Wip branches

- `wip/mac-4d96/retire-ask` @ 4fcd9df61 (on #3202's `cross-wake` afe067115): park's test with the word up
  also reads `E_RETIRED`, so a retire ask that finds a spare awake is answered. **Unproven:** the race never
  showed on the Mac, even widened. Held until the coordinator's Windows runs (main vs main + #3202) say
  whether it is the cause of the train-15 rescue failures; a second hypothesis is a spare left at
  `R_DECLINED`. Analysis in crew #135 notes. #3202 itself (afe067115) is mac-60d0's line, waiting.
- `wip/mac-4d96/tools` (this file, scripts and probe programs under `notes/mac-4d96/`, scratch helpers under
  `notes/mac-4d96/scratch/`).

## Next steps

1. **#3420 arm64 rows** when slot B is given by name: in the VM (`orb -m velt-prof`),
   `SRC=<main-base> bash vm.sh build`, then
   `bash vm.sh reps spawn_many 4 12 p23a p23b` and the same for `pingpong_pinned`, `pingpong_colocated`
   (base = `branches/main-base` at c88e04161, the PR's main; `p23a` = std of 083578e04, `p23b` = std of
   661b95982; both trees are under `branches/sleep-combo-var/`). Then `vm.sh vars "<rows>" p23a p23b` at T1.
   Post on #3420, then `vm.sh clean` and `heavy done`.
2. **#3273:** act on the owner's ruling. If it is "cut", the two park/inbox variants measured nothing above
   the spread on arm64 (`sleep-combo-var/r1-nowake`, `r1-notick`); profile per function at T4 on x86.
3. **#3369:** answer the re-review. Tell mac-166e (#3390) and mac-77e0 (#3411) first if `resolver()` /
   `rejecter()` in `std/lang/promise/settle.velt` change.

## Measurements and probes

- Nothing running. The VM holds nothing of mine (cleaned after each turn).
- Scripts: `notes/mac-4d96/vm.sh` (VM cachegrind: `build`, `vars`, `reps` interleaved min/median, `prof`,
  `annot`, `clean`; `SRC=` picks the compiler source), `checks3323.sh` (Mac checks of #3420 in sleep-home
  detached), `checks3360.sh` (same for #3369), `checksmain.sh`, `p3323/run.sh` (base vs head probes, counted),
  `p3323/par.sh` (a probe J at a time, for load), `probes/plain.sh` (uncounted runs), `probes/stress.sh`.
- #3420 probes: `p3323/{handed_drop,t_finished,t_two,t_mixed,t_race}.velt` (the test file is generated from
  them by `scratch/mktest3323.py`). Break-check stds: `p3323/mkbreak*.py`, `mkwake.py`.

## Do not redo

- The `S_OWNER` split onto main for #3273 cannot work (main has no hand-over; it crashes from #3207 itself).
- #3389's cause is in lowering (`needsTransfer` lowered with promises: false), not in std; don't dig in
  `sched_share_hand.velt`.
- Run std/scheduler programs **plain** (no `VELT_RT_DEBUG_ALLOC`) as well as counted: the checking allocator
  never reuses an address and hid #3273's crash.
- Mac quirk: suite child programs sometimes stall in the dynamic loader (`_dyld_start`, or an `fcntl` while
  mapping the shared runtime) at 0% CPU. `sample <pid>` shows it; kill by PID and rerun that test by name.

## Build dirs

- Keep warm: `branches/sleep-home/target` (16 GB): debug build of the compiler at main c88e04161, used for
  std-only runs of every branch above (`VELT_STD=<std>`; the `sleep-home` worktree is checked out detached
  at a PR branch by the check scripts and switched back after). `sleep-home` itself is on #3273's branch.
- Throwaway: `branches/sleep-combo-var/` (scratch std copies and built probes; its scripts are on the tools
  branch). `cross-wake/target` was cleaned (needs a fresh build on today's main). Local branches
  `sleep-home-keep`, `parked-away-wip`, `parked-task`, `sleep-combo` can be deleted.
