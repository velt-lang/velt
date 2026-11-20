#!/bin/bash
# The #3274 golden with each break mutant (mut.py), N runs per mode (run.sh): each must fail.
#   bash breaks.sh [runs=3]
cd "$(dirname "$0")" || exit 1
runs=${1:-3}
SRC=/Users/andii/dev/velt/branches/parked-task/std
for k in ${MUTS:-gate nested joinparked eager}; do
  rm -rf "std-$k"; cp -R "$SRC" "std-$k"
  python3 "$PWD/mut.py" "$PWD/std-$k" "$k" || exit 1
  bash run.sh "$k" "$PWD/std-$k" "$runs"
  diff "out.$k.txt" /Users/andii/dev/velt/branches/parked-task/tests/golden/lang/task_parked_freed.out | head -6
done
