#!/bin/bash
# timers_due_stored and timers (1 worker, min of 3 runs, instructions) with each attribution
# variant of the std against the base std.
S=$(cd "$(dirname "$0")" && pwd)
T=/Users/andii/dev/velt/branches/cross-wake/target/debug
B=/Users/andii/dev/velt/branches/cross-wake/bench/async
export SDKROOT=$(xcrun --show-sdk-path)
python3 "$S/attrib.py" "$S/av" > /dev/null || exit 1
count() {
  local best=0
  for i in 1 2 3; do
    n=$( { VELT_THREADS=1 /usr/bin/time -l "$1" > /dev/null; } 2>&1 | awk '/instructions retired/ {print $1}')
    if [ "$best" = 0 ] || [ "$n" -lt "$best" ]; then best=$n; fi
  done
  echo "$best"
}
for row in timers_due_join; do
  VELT_STD=/Users/andii/dev/velt/branches/timer-unanchor/std "$T/velt" build --release "$B/$row.velt" -o "$S/rows/a-$row-base" 2> /dev/null
  b=$(count "$S/rows/a-$row-base")
  echo "$row base $b"
  for v in full nopark nodropasks nocheck nopoll plainstores none; do
    VELT_STD=$S/av-$v "$T/velt" build --release "$B/$row.velt" -o "$S/rows/a-$row-$v" 2> "$S/rows/a-$row-$v.err" || { echo "$row $v: build failed"; head -3 "$S/rows/a-$row-$v.err"; continue; }
    h=$(count "$S/rows/a-$row-$v")
    echo "$row $v: $(( (h - b) / 1000 ))k over base ($(echo "scale=2; ($h - $b) * 100 / $b" | bc)%)"
  done
done
