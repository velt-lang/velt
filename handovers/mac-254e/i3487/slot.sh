#!/bin/bash
# #3487 slot A: rebuild on main, scheduler tests touched by the change, then the gate.
set -u
cd $HOME/dev/velt/branches/fix-3420-254e || exit 1
export CARGO_TARGET_DIR=$PWD/target CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 VELT_STD=$PWD/std
L=$HOME/dev/velt/branches/r3420-254e/i3487/slot
mkdir -p "$L"
echo "build start $(date +%T)"
cargo build -q --workspace > "$L/build.log" 2>&1 || { echo "BUILD FAILED"; tail -20 "$L/build.log"; exit 1; }
echo "build done $(date +%T)"
for t in std_scheduler_shared_parked std_scheduler_parked_away std_scheduler_handed_promises std_scheduler_handover std_scheduler_rescue std_scheduler_catch std_scheduler_wake_end std_scheduler_bounds; do
  r=$(cargo test -q -p veltc --test "$t" 2>&1 | tee "$L/$t.log" | grep -E "test result|FAILED" | tr '\n' ' ')
  echo "$t: $r"
done
echo "gate start $(date +%T)"
scripts/check.sh > "$L/gate.log" 2>&1
echo "gate exit $? $(date +%T)"
tail -15 "$L/gate.log"
