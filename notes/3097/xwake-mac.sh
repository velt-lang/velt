#!/usr/bin/env bash
# The c=1 cross-worker round trip on this Mac: step 2's std (base) vs the working tree's (head),
# one compiler (cross-wake's debug velt, release runtime), interleaved.
set -uo pipefail
WT=/Users/andii/dev/velt/branches/cross-wake
BASE_STD=/Users/andii/dev/velt/branches/timer-unanchor/std
OUT=/private/tmp/claude-501/-Users-andii-dev-velt-main/505bcb47-021f-409e-bf01-c29e5c3a75e5/scratchpad/xw
mkdir -p "$OUT"
SRC=/Users/andii/dev/velt/branches/bench-2944/xwake.velt
export SDKROOT=$(xcrun --show-sdk-path)
export VELT_RT_LIB=$WT/target/release/libvelt_rt.a
VELT_STD=$BASE_STD "$WT/target/debug/velt" build --release "$SRC" -o "$OUT/xw-base" || exit 1
VELT_STD=$WT/std "$WT/target/debug/velt" build --release "$SRC" -o "$OUT/xw-head" || exit 1
for r in 1 2 3; do
  for s in base head; do
    echo "== $s"
    VELT_THREADS=${THREADS:-4} "$OUT/xw-$s"
  done
done
