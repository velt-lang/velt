#!/usr/bin/env bash
# Builds flip-probe.velt with lazy-output-order's compiler and runs it 30 times at VELT_THREADS=1
# VELT_RESCUE=1 VELT_RESCUE_STATS=1: per run, the workers main slept on and the locks-on count.
set -uo pipefail
WT=/Users/andii/dev/velt/branches/lazy-output-order
OUT=/private/tmp/claude-501/-Users-andii-dev-velt-main/505bcb47-021f-409e-bf01-c29e5c3a75e5/scratchpad/flip
mkdir -p "$OUT"
VELT_SCHEDULER=std "$WT/target/debug/velt" build /Users/andii/dev/velt/branches/bench-2944/flip-probe.velt -o "$OUT/flip" || exit 1
for i in $(seq 30); do
  VELT_THREADS=1 VELT_RESCUE=1 VELT_RESCUE_STATS=1 "$OUT/flip" >/dev/null 2>"$OUT/err"
  echo "run $i: sleeps on [$(grep -o 'SLEEP_ON [0-9]*' "$OUT/err" | awk '{print $2}' | tr '\n' ' ')] $(grep -o 'locks-on=[0-9]*' "$OUT/err")"
done
