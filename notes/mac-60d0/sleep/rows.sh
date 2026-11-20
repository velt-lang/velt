#!/bin/bash
# Instruction counts (macOS, min of 3) of the timer rows: base = timer-unanchor's std, head =
# sleep-home's, built --release by cross-wake's velt.   bash rows.sh [row...]
S=$(cd "$(dirname "$0")" && pwd)
W=/Users/andii/dev/velt/branches/sleep-home
T=/Users/andii/dev/velt/branches/sleep-home/target/debug
export SDKROOT=$(xcrun --show-sdk-path)
mkdir -p "$S/rows"
count() {
  local best=0
  for i in 1 2 3; do
    n=$( { VELT_THREADS=$2 /usr/bin/time -l "$1" > /dev/null; } 2>&1 | awk '/instructions retired/ {print $1}')
    if [ "$best" = 0 ] || [ "$n" -lt "$best" ]; then best=$n; fi
  done
  echo "$best"
}
B=/Users/andii/dev/velt/branches/sleep-home/bench/async
rows=("$@"); [ ${#rows[@]} = 0 ] && rows=(timers timers_due_stored timers_due_reversed timers_due_join timers_due_in_promise)
for row in "${rows[@]}"; do
  VELT_STD=/Users/andii/dev/velt/branches/main-base/std "$T/velt" build --release "$B/$row.velt" -o "$S/rows/$row-base" 2> /dev/null || echo "$row base build failed"
  VELT_STD=$W/std "$T/velt" build --release "$B/$row.velt" -o "$S/rows/$row-head" 2> /dev/null || echo "$row head build failed"
  for th in 1 4; do
    b=$(count "$S/rows/$row-base" $th); h=$(count "$S/rows/$row-head" $th)
    echo "$row T=$th: base $b head $h $(echo "scale=2; ($h - $b) * 100 / $b" | bc)% ($(( (h - b) / 1000 ))k)"
  done
done
