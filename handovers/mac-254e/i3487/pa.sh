#!/bin/bash
# Reruns std_scheduler_parked_away's "dropped as it finishes" test with the branch std and main's.
set -u
W=$HOME/dev/velt/branches/fix-3420-254e
M=$HOME/dev/velt/branches/r3420-254e/std-main
rm -rf "$M" && mkdir -p "$M" && (cd "$W" && git archive origin/main std | tar -x -C "$M" --strip-components=1)
cd "$W" || exit 1
export CARGO_TARGET_DIR=$PWD/target CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
T=a_promise_dropped_as_it_finishes_lets_its_parked_task_go
for s in branch main; do
  if [ $s = branch ]; then S=$W/std; else S=$M; fi
  bad=0
  for i in 1 2 3 4 5 6 7 8 9 10; do
    if ! VELT_STD=$S cargo test -q -p veltc --test std_scheduler_parked_away "$T" > $SCRATCH/pa-$s.log 2>&1; then
      bad=$((bad + 1)); grep -m1 "blocks=" $SCRATCH/pa-$s.log | cut -c1-160
    fi
  done
  echo "$s: $bad of 10 failed"
done
