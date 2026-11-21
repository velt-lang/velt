#!/bin/bash
# A probe (default pinned_due) with the fixed std and release mutants, N runs each (probe1.sh).
#   bash due.sh [runs=10] [probe=pinned_due] [mutants...]
cd "$(dirname "$0")" || exit 1
runs=${1:-10}; probe=${2:-pinned_due}; shift 2 2>/dev/null
muts=("$@"); [ ${#muts[@]} = 0 ] && muts=(due fired)
FIX=/Users/andii/dev/velt/branches/sleep-home/std
bash probe1.sh fix "$FIX" "$probe" "$runs"
for k in "${muts[@]}"; do
  rm -rf "std-mut-$k"; cp -R "$FIX" "std-mut-$k"
  python3 "$PWD/mutrel.py" "$PWD/std-mut-$k" "$k" || exit 1
  bash probe1.sh "mut-$k" "$PWD/std-mut-$k" "$probe" "$runs"
done
