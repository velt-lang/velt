#!/bin/bash
# sleep-home's std checked without a build of its own: cross-wake's test binaries (same compiler
# crates; its own gather/spin tests left out, they need cross-wake's std) with VELT_STD = the
# fixed std, the handed-on probes as stress, and instruction counts of the timer rows against
# the base std (timer-unanchor), built --release by cross-wake's velt, min of 3.
S=$(cd "$(dirname "$0")" && pwd)
W=/Users/andii/dev/velt/branches/sleep-home
T=/Users/andii/dev/velt/branches/cross-wake/target/debug
export SDKROOT=$(xcrun --show-sdk-path) VELT_ZIG=$HOME/.velt/cache/zig/0.17.1/zig-aarch64-macos-0.17.1/zig
mkdir -p "$S/logs" "$S/rows"
cd /Users/andii/dev/velt/branches/cross-wake/crates/veltc || exit 1
for t in std_scheduler std_scheduler_timers std_scheduler_carry std_scheduler_workers std_scheduler_order std_scheduler_bounds std_scheduler_rescue std_scheduler_goldens std_scheduler_handover std_scheduler_bridge std_scheduler_exit_order c_export_async; do
  BIN=$(ls -t "$T"/deps/$t-* | grep -v "$t-[0-9a-f]*\\." | head -1)
  VELT_STD=$W/std "$BIN" --test-threads 3 > "$S/logs/$t.log" 2>&1; echo "$t exit $? $(grep 'test result' "$S/logs/$t.log" | tail -1 | cut -c1-60)"
done
bash "$S/stress.sh" fix2 "$W/std" 20 handed4 handed5 2>&1 | cut -c1-120
count() {
  local best=0
  for i in 1 2 3; do
    n=$( { VELT_THREADS=$2 /usr/bin/time -l "$1" > /dev/null; } 2>&1 | awk '/instructions retired/ {print $1}')
    if [ "$best" = 0 ] || [ "$n" -lt "$best" ]; then best=$n; fi
  done
  echo "$best"
}
B=/Users/andii/dev/velt/branches/cross-wake/bench/async
for row in timers timers_fire timers_fire_phases timers_due_stored timers_due_reversed timers_due_join timers_due_in_promise race_select; do
  VELT_STD=/Users/andii/dev/velt/branches/timer-unanchor/std "$T/velt" build --release "$B/$row.velt" -o "$S/rows/$row-base" 2> /dev/null || echo "$row base build failed"
  VELT_STD=$W/std "$T/velt" build --release "$B/$row.velt" -o "$S/rows/$row-head" 2> /dev/null || echo "$row head build failed"
  for th in 1 4; do
    b=$(count "$S/rows/$row-base" $th); h=$(count "$S/rows/$row-head" $th)
    echo "$row T=$th: base $b head $h $(echo "scale=2; ($h - $b) * 100 / $b" | bc)%"
  done
done
