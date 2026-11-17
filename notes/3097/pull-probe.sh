#!/usr/bin/env bash
# pull-probe.velt with the std in $1 (default: lazy-output-order's), 10 runs at VELT_THREADS=1
# VELT_RESCUE=1: the locks-on count per run (>= 2: worker 0 was lazy again).
set -uo pipefail
WT=/Users/andii/dev/velt/branches/lazy-output-order
STD=${1:-$WT/std}
OUT=/private/tmp/claude-501/-Users-andii-dev-velt-main/505bcb47-021f-409e-bf01-c29e5c3a75e5/scratchpad/flip
mkdir -p "$OUT"
VELT_STD=$STD VELT_SCHEDULER=std "$WT/target/debug/velt" build /Users/andii/dev/velt/branches/bench-2944/pull-probe.velt -o "$OUT/pull" || exit 1
for i in $(seq 10); do
  VELT_THREADS=1 VELT_RESCUE=1 VELT_RESCUE_STATS=1 "$OUT/pull" >/dev/null 2>"$OUT/perr"
  echo "run $i: $(grep -o 'rescues=[0-9]* spares=[0-9]* locks-on=[0-9]*' "$OUT/perr")"
done
