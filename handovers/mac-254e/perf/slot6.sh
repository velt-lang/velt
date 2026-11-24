#!/bin/bash
# #3498 A' final checks after the out-of-line letGo and ownSide's task 0: the held tests, the
# scheduler suites, precheck, then the gate.
set -u
W=$HOME/dev/velt/branches/fix-3420-254e
P=$HOME/dev/velt/branches/r3420-254e/perf
cd "$W" || exit 1
export CARGO_TARGET_DIR=$W/target CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
for t in std_scheduler_held std_scheduler_parked_away std_scheduler_handed_promises std_scheduler_handover std_scheduler_rescue std_scheduler_timers std_scheduler_waiting_in_hub std_scheduler_shared_parked std_scheduler_catch std_scheduler_yield std_scheduler_fair std_scheduler_bounds std_scheduler_wake_end; do
  echo "$t: $(cargo test -q -p veltc --test $t 2>&1 | grep -E 'test result|FAILED' | tr '\n' ' ' | cut -c1-90)"
done
cargo xtask precheck 2>&1 | grep -E "precheck (passed|FAILED)|lines besides"
echo "== gate $(date +%T)"
scripts/check.sh > "$P/gate7.log" 2>&1
echo "gate exit $?"
grep -v "^\s*PASS" "$P/gate7.log" | tail -10
echo "== done $(date +%T)"
