#!/bin/bash
# Mac instruction counts (min of 5) for timer rows with three std trees: main-base, a given
# older sleep-home commit, and sleep-home's working tree. One codegen unit.
#   bash rows3.sh <old-commit> [row...]
S=$(cd "$(dirname "$0")" && pwd)
W=/Users/andii/dev/velt/branches/sleep-home
V=$W/target/debug/velt
old=$1; shift
export SDKROOT=$(xcrun --show-sdk-path) VELT_CODEGEN_UNITS=1
rm -rf "$S/std-old" "$S/rows3"; mkdir -p "$S/std-old" "$S/rows3"
git -C "$W" archive "$old" std | tar -x -C "$S/std-old"
count() {
  local best=0
  for i in 1 2 3 4 5; do
    n=$( { VELT_THREADS=1 /usr/bin/time -l "$1" > /dev/null; } 2>&1 | awk '/instructions retired/ {print $1}')
    if [ "$best" = 0 ] || [ "$n" -lt "$best" ]; then best=$n; fi
  done
  echo "$best"
}
rows=("$@"); [ ${#rows[@]} = 0 ] && rows=(timers_due_stored timers_due_join)
for row in "${rows[@]}"; do
  VELT_STD=/Users/andii/dev/velt/branches/main-base/std "$V" build --release "$W/bench/async/$row.velt" -o "$S/rows3/$row-base" 2> /dev/null || echo "base build failed"
  VELT_STD=$S/std-old/std "$V" build --release "$W/bench/async/$row.velt" -o "$S/rows3/$row-old" 2> /dev/null || echo "old build failed"
  VELT_STD=$W/std "$V" build --release "$W/bench/async/$row.velt" -o "$S/rows3/$row-head" 2> /dev/null || echo "head build failed"
  b=$(count "$S/rows3/$row-base"); o=$(count "$S/rows3/$row-old"); h=$(count "$S/rows3/$row-head")
  echo "$row T=1: base $b  old +$(( (o - b) / 1000 ))k  head +$(( (h - b) / 1000 ))k"
done
