#!/usr/bin/env bash
# Cold-cycle alone vs after the fan rows: base (merge base std) vs s2 (step 2) vs head (steps 1+2),
# the actors app built with cross-wake's compiler, 4 shards, on this Mac.
set -uo pipefail
WT=/Users/andii/dev/velt/branches/cross-wake
OUT=/private/tmp/claude-501/-Users-andii-dev-velt-main/505bcb47-021f-409e-bf01-c29e5c3a75e5/scratchpad/xw
mkdir -p "$OUT"
BASE_STD=/private/tmp/claude-501/-Users-andii-dev-velt-main/505bcb47-021f-409e-bf01-c29e5c3a75e5/scratchpad/base/std
std_of() {
  case $1 in
    base) echo "$BASE_STD" ;;
    s2) echo /Users/andii/dev/velt/branches/timer-unanchor/std ;;
    head) echo "$WT/std" ;;
  esac
}
export SDKROOT=$(xcrun --show-sdk-path)
export VELT_RT_LIB=$WT/target/release/libvelt_rt.a
for s in base s2 head; do
  (cd "$WT/examples/apps/actors" && VELT_STD=$(std_of $s) "$WT/target/debug/velt" build --release -o "$OUT/actors-$s" >/dev/null 2>&1) || echo "build $s failed"
done
for r in 1 2; do
  for s in base s2 head; do
    echo "== $s cold only"; "$OUT/actors-$s" bench --duration 1000 --shards 4 --only cold | grep -A1 cold-cycle | tail -1
    echo "== $s fan,cold"; "$OUT/actors-$s" bench --duration 500 --shards 4 --only fan,cold | grep -A1 cold-cycle | tail -1
  done
done
