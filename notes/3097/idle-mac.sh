#!/usr/bin/env bash
# CPU time of idle.velt (5 s, ticker every 10 ms, 4 workers): base (step 2 std) vs head (cross-wake
# std), interleaved, from /usr/bin/time -l (user, sys seconds).
set -uo pipefail
WT=/Users/andii/dev/velt/branches/cross-wake
BASE_STD=/Users/andii/dev/velt/branches/timer-unanchor/std
OUT=/private/tmp/claude-501/-Users-andii-dev-velt-main/505bcb47-021f-409e-bf01-c29e5c3a75e5/scratchpad/xw
mkdir -p "$OUT"
SRC=/Users/andii/dev/velt/branches/bench-2944/idle.velt
export SDKROOT=$(xcrun --show-sdk-path)
export VELT_RT_LIB=$WT/target/release/libvelt_rt.a
VELT_STD=$BASE_STD "$WT/target/debug/velt" build --release "$SRC" -o "$OUT/idle-base" || exit 1
VELT_STD=$WT/std "$WT/target/debug/velt" build --release "$SRC" -o "$OUT/idle-head" || exit 1
for r in 1 2; do
  for s in base head; do
    /usr/bin/time -p env VELT_THREADS=4 "$OUT/idle-$s" 2>"$OUT/idle.time" >/dev/null
    echo "$s: $(tr '\n' ' ' < "$OUT/idle.time")"
  done
done
