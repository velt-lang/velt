#!/usr/bin/env bash
# Run time of the printing programs (`run_*.velt`) built by two compilers, by wall clock, for a
# machine without an instruction counter (Windows): each program is built by both with FLAGS
# (default `--release`; `FLAGS=` for a debug build), then the two binaries run in turns, RUNS
# times each, output to /dev/null, and the best time of each is printed with the difference.
#
#   bench/inspect/wall.sh WORK BASE_VELT BASE_STD HEAD_VELT HEAD_STD [program…]
#
# A program is a file name below bench/inspect without `.velt` (default: every `run_*`).
set -euo pipefail
[ $# -ge 5 ] || { echo "usage: bench/inspect/wall.sh WORK BASE_VELT BASE_STD HEAD_VELT HEAD_STD [program…]" >&2; exit 2; }
WORK=$1 BASE=$2 BASE_STD=$3 HEAD=$4 HEAD_STD=$5
shift 5
HERE=$(cd "$(dirname "$0")" && pwd)
RUNS=${RUNS:-5}
FLAGS=${FLAGS-"--release"}
mkdir -p "$WORK"
progs=("$@")
if [ ${#progs[@]} -eq 0 ]; then
  for f in "$HERE"/run_*.velt; do progs+=("$(basename "$f" .velt)"); done
fi

ms() { echo $(( $(date +%s%N) / 1000000 )); }

for p in "${progs[@]}"; do
  VELT_STD=$BASE_STD "$BASE" build $FLAGS "$HERE/$p.velt" -o "$WORK/$p.base.exe" > /dev/null
  VELT_STD=$HEAD_STD "$HEAD" build $FLAGS "$HERE/$p.velt" -o "$WORK/$p.head.exe" > /dev/null
  best_b=999999999 best_h=999999999
  for _ in $(seq "$RUNS"); do
    for side in base head; do
      t0=$(ms); "$WORK/$p.$side.exe" > /dev/null; t=$(( $(ms) - t0 ))
      if [ $side = base ] && [ $t -lt $best_b ]; then best_b=$t; fi
      if [ $side = head ] && [ $t -lt $best_h ]; then best_h=$t; fi
    done
  done
  awk -v p="$p" -v b="$best_b" -v h="$best_h" \
    'BEGIN { printf "%-12s base %6d ms  head %6d ms  %+.1f%%\n", p, b, h, (h - b) * 100 / b }'
done
