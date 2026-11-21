#!/bin/bash
# The new test binary of sleep-home: the stress test N at once, R rounds; then c_export_async alone.
L=$(cd "$(dirname "$0")" && pwd)/rerun
mkdir -p "$L"
W=/Users/andii/dev/velt/branches/sleep-home
export SDKROOT=$(xcrun --show-sdk-path) VELT_ZIG=$HOME/.velt/cache/zig/0.17.1/zig-aarch64-macos-0.17.1/zig
cd "$W/crates/veltc" || exit 1
BIN=$(ls -t "$W"/target/debug/deps/std_scheduler_sleep_handed-* | grep -v "handed-[0-9a-f]*\\." | head -1)
n=${1:-8}; rounds=${2:-3}
for r in $(seq 1 "$rounds"); do
  pids=(); fails=0
  for i in $(seq 1 "$n"); do
    "$BIN" --exact handed_on_sleeps_fire_under_offers > "$L/h-$r-$i.log" 2>&1 &
    pids+=($!)
  done
  for p in "${pids[@]}"; do wait "$p" || fails=$((fails + 1)); done
  echo "round $r: $fails of $n failed"
done
grep -h "panicked" -A1 "$L"/h-*.log | grep -v "^--" | cut -c1-160 | sort | uniq -c | head -5
C=$(ls -t "$W"/target/debug/deps/c_export_async-* | grep -v "async-[0-9a-f]*\\." | head -1)
"$C" > "$L/c_export.log" 2>&1; echo "c_export_async alone: exit $? $(grep 'test result' "$L/c_export.log" | cut -c1-60)"
