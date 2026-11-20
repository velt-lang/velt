#!/bin/bash
# #3289 cuts on the Mac (mac-77e0), in this worktree at its HEAD with its own target: build, the
# yield goldens direct at 1/4 threads, the yield/sleep/timer/promise/race/spawn goldens,
# std_scheduler suites, std tests, break checks on a copy of std, precheck. Usage: job14.sh <log>
set -u
cd ~/dev/velt/branches/yield-order-fix
export CARGO_BUILD_JOBS=3 SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk CARGO_TARGET_DIR=$PWD/target VELT_STD=$PWD/std
L=$1
S=${S:-/private/tmp/claude-501/job14}; mkdir -p $S
V=$PWD/target/debug/velt
step() { echo "== $* $(date +%H:%M:%S)"; }
to() { local s=$1; shift; perl -e "alarm $s; exec @ARGV" "$@"; }
G="yield_now_immediates yield_now_root_after_turn yield_now_due_sleep yield_now_turn_passed yield_now_order yield_now_stored yield_now_stored_sync"
{
step "build at $(git log --oneline -1 | cut -c1-60)"
cargo build -q --workspace 2>&1 | grep -E "^(error|warning)" | head; echo "build exit ${PIPESTATUS[0]}"
step "yield goldens direct (1/4 threads)"
for g in $G; do
  for th in 1 4; do
    VELT_THREADS=$th to 60 $V run tests/golden/lang/$g.velt > $S/$g.$th.out 2>&1
    cmp -s $S/$g.$th.out tests/golden/lang/$g.out && echo "$g t$th same" || { echo "$g t$th DIFF"; diff $S/$g.$th.out tests/golden/lang/$g.out | head -6; }
  done
done
step "goldens"
for g in yield sleep timer immediate promise race spawn task_ catch_panic uncaught; do
  r=$(VELT_GOLDEN=$g cargo test --workspace --test golden 2>&1 | tee -a $L.golden | grep -E "test result: FAILED|differ|mismatch|want [0-9]+\)" | tail -6 | tr '\n' ' ')
  echo "$g: ${r:-ok}"
done
step "std_scheduler suites"
for f in crates/veltc/tests/std_scheduler*.rs; do
  n=$(basename $f .rs)
  [ "$n" = std_scheduler_windows_link ] && continue
  r=$(cargo test -p veltc --test $n 2>&1 | grep -E "test result|FAILED|panicked" | tail -3 | tr '\n' ' ')
  echo "$n: $r"
done
step "std tests"
cargo test -p veltc --test std_tests 2>&1 | grep -E "test result|FAILED|panicked" | tail -4
step "break checks on a copy of std"
B=$S/std-break
mutant() { # name file sed-expr goldens...
  local name=$1 file=$2 expr=$3; shift 3
  B=$S/std-break-$name; [ -d $B ] && { echo "$name: copy exists, skipped"; return; }; cp -R std $B
  sed -i '' "$expr" $B/lang/hooks/$file
  if cmp -s std/lang/hooks/$file $B/lang/hooks/$file; then echo "$name: NOT APPLIED"; return; fi
  for g in "$@"; do
    VELT_STD=$B to 60 $V run tests/golden/lang/$g.velt > $S/brk.$name.$g.out 2>&1
    cmp -s $S/brk.$name.$g.out tests/golden/lang/$g.out && echo "$name $g: same" || echo "$name $g: DIFF"
  done
}
mutant no-root-record sched_yield_list.velt 's/  if (last === 0 \&\& t.has(k, t.ROOT_ALONE) \&\& m.load32(k + t.T_STATE) === t.YIELDED) {/  if (false) {/' yield_now_turn_passed
mutant root-always-ready sched_yield.velt 's/    return m.load32(w + t.T_STATE) === t.YIELDED ? 0 : 1;/    return 1;/' yield_now_order yield_now_root_after_turn
mutant no-dueNow sched_fire.velt 's/^  if (dueNow(e, k)) {/  if (false \&\& dueNow(e, k)) {/' yield_now_due_sleep
mutant no-lazy-join sched_yield.velt 's/^  if (s === ls.NEW) {/  if (false \&\& s === ls.NEW) {/' yield_now_immediates yield_now_turn_passed
mutant no-generation sched_yield.velt 's/^  } else if (m.ld(f + ls.Y_STAMP) \/ ls.GEN !== m.ld(m.ld(k + t.T_YIELDS) + ls.B_STAMP) \/ ls.GEN) {/  } else if (false) {/' yield_now_turn_passed yield_now_stored
mutant no-firing sched_yield_list.velt 's/QUEUED, FIRING) !== QUEUED/QUEUED, FIRED) !== QUEUED/' yield_now_stored
mutant no-reacts sched_fire.velt 's/^  return micro.pending(k) || m.ld(k + t.T_UNHANDLED) !== 0;/  return false;/' yield_now_immediates yield_now_order
mutant no-woken-between sched_fire.velt 's/^  if (m.load(k + t.T_WOKEN) !== 0) {/  if (false) {/' yield_now_immediates
mutant no-root-woke sched_fire.velt 's/^    return rootWoke(e, k);/    return woken;/' sleep_turns_root_between
# Not a break check: without the own-cx test the table test must give the same order.
mutant own-cx-never sched_yield.velt 's/^  if (cx === w + t.T_CX) {/  if (false) {/' yield_now_order yield_now_immediates
# A mutant's program that hung outlives its alarm (the alarm ends `velt run`, not the program):
# those left are stopped by PID.
for pid in $(pgrep -f "$PWD/target/velt/yield_now_"); do kill $pid 2>/dev/null; done
step "precheck"
cargo xtask precheck 2>&1 | tail -4
step "done"
} > $L 2>&1
