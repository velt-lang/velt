# Handoff: mac-9a21, crew task #124 (#2943 compilation units), 2026-11-20

Task #124 (from win-coordinator): take #2943's units work (std checked as its own unit,
frozen, saved, then programs built from it) from the experimental branch `units-passes` to
reviewable PRs, opt-in behind `VELT_UNITS` until the end. The owner's goal is faster CI and
builds. #2943's checklist comment has the series plan (U1..U7).

## State of the series

Merged on main:
- U1a-U1e: the unit's encoding (#3254, #3265, #3266, #3280, #3287, #3288), train 14.
- U2a #3269 (read-only scans), U2b #3292 (passes look at the unit being checked).
- U3a #3294 (`VELT_UNITS=2`: std checked as a check of its own, the app from a copy).
- #3311 (lib.rs's analysis functions moved to analysis.rs).

Open, mine:

| PR | branch | head | base | state |
|---|---|---|---|---|
| #3306 U3b: std's JSON-needs summary | `units-sums-1` | 22e613629 | main | APPROVED; rebased after train 14 with json_needs saved in the unit; check.sh green; waiting for a train |
| #3335 U3c: std's unit over std's modules alone, no generated code, ends clean | `units-std-clean` | e75db7ceb | `units-sums-1` | opened 2026-11-20, awaiting review + A/B |
| #3336 U4: std's unit kept on disk (`VELT_UNITS_DISK`) and loaded for other programs | `units-disk` | 7a2dbc5c1 | `units-std-clean` | opened 2026-11-20, awaiting review + A/B |

Done: check.sh at #3336's exact head 7a2dbc5c1 (holds #3335) passed (2416 s), posted on both PRs.
The review agent runs the
default-mode A/B by instructions retired: #3306 22e613629 vs #3335 e75db7ceb, and #3335 vs
#3336 7a2dbc5c1. Post the numbers in each self-check's item 5.

When #3306 lands, retarget #3335 to main (GitHub does it when the base branch is deleted), and
#3336 follows #3335.

## Measured (2026-11-20, this Mac)

Over the 931 errors goldens (`tests/golden/**/errors/*.velt` with a `.err`), `velt check`:
- `VELT_UNITS=2` from disk vs in process: same diagnostics and exit code on all 931; 878 load
  std's unit from disk (the others' source-scan flags give another key).
- `VELT_UNITS=2` vs a whole check (VELT_UNITS unset): 42 differ (was 117 before U3c). The list is
  `notes/diff-whole.txt` on the wip branch; these are passes whose summaries of std's unit are
  still to come (rwlock_read_*, extended_layouts_*, mutex_with_*, readonly_*, http_server's ...).
- Loading std's unit: ~5 ms for 10 MB.

Rerun: `notes/u4_goldens.py` (a velt built at the head under test, the worktree checked out at
that head so `VELT_STD=<worktree>/std` matches). It prints both counts and writes diff-whole.txt.

## What's next, in order

1. Land #3306, #3335, #3336 (answer reviews; the A/B must be within ±3% by instructions).
2. Per-pass summaries for the 42 goldens, family by family, as #3306 did for JSON. Review
   follow-ups that bind this:
   - the record-keys and struct-tuple summaries go into the step that makes those passes look at
     the app only (no program can observe them before: #3306's review);
   - `ctx/mod.rs` is at 799/800 lines: put any new summary field outside it (a struct in
     `units`, e.g. `units::Summaries` as on `units-passes`).
3. U5: build programs from std's unit (on `units-passes`: `VELT_UNITS_BUILD=1`; per-program build
   cost was the problem: reach, `finalize::build_defs`, read-only erase, unit decode; move them
   into the unit). Then the goldens in both modes in CI, U6 (the two parked goldens:
   `http_handler_changes_through_call`, `db_rows_source_throws`), U7 default on, then the
   lowering-cache design note.

## Traps (each cost real time)

- The unit encoding lists every variant of HIR/defs enums and destructures every `Ctx` field.
  Any PR adding a variant or a `Ctx` field must add it to `crates/velt_sema/src/units/codec/hir.rs`,
  `defs.rs`, or `unit.rs` (`unit_fields!`: save / special / skip / memo / empty), or main fails to
  compile in the train. The coordinator told train-15 authors; offer help.
- `unit_fields!` groups: `memo` fields are cleared when std's stage ends (`clear_memos`), and
  `empty` ones are tested empty at a unit's end. A field that std's stage leaves set must not
  be in `empty` (`collected_defs` was; the test caught it).
- An in-process copy of std's unit must equal a loaded one: memos built over std alone were
  stale for the app and gave spurious errors (fixed by `units::finish_std`). Compare with
  `notes/u4_goldens.py` after any change to std's stage.
- velt_sema bans std `HashMap`/`HashSet` even in tests (`hasher_tests`): use Fx types.
- A break check that does not compile tests nothing; one whose program the test does not reach
  survives. Every new test here was checked with `notes/mutants.py --list <module>`.
- Rebasing after a train: main moves fast; re-run the U-stack gate right before a train.
- The units crossing test's program (crates/veltc/tests/units.rs `crossings()`) overrides a std
  method without a slot; mac-4c53 is changing it in #3301 because std gave `Error.toString` a
  slot. Check the new program still has no velt-units-free run.

## Local state on the Mac

- `~/dev/velt/branches/d1-open-pr`: worktree on `units-passes` (f5d026436, pushed), with the
  build directory `target/` (~20 GB) all my gates used. Keep it if the next session continues
  #2943 (warm builds); otherwise delete the worktree and target.
- `~/dev/velt/branches/units-u1a`: worktree I edited in (no build dir of its own; `notes/chk-u1.sh`
  type-checks it against d1-open-pr's target). Local branches there besides the PR branches
  (`u3c`, `u4-main`, `rb-*`, `u3c-pr`, `u4-pr`) are superseded copies; safe to delete.
- Earlier handover notes from the previous owner: `branches/notes-2943`,
  `branches/handovers/mac-e688-2943` (val logs, `gold-both.sh`, `pr-ab.sh`).
- The scripts I used are on `wip/mac-9a21/units` under `notes/` (paths inside point at my
  session's scratchpad: adjust `S=` and `W=`).

## Crew habits that worked

- Own heavy slot for #2943: post `heavy start ...` / `heavy done`; the coordinator lends it only
  after you say "free". `CARGO_BUILD_JOBS=3`, one heavy job at a time, df >= 30 GiB.
- Gates for PR branches ran in d1-open-pr via `git checkout --detach <branch>`, then back to
  `units-passes`.
