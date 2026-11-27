#!/usr/bin/env bash
# Instructions (cachegrind `I refs`) of programs built with one compiler against two standard
# libraries: for a change whose runtime effect is std code (CLAUDE.md "Measuring on a busy
# machine": std is read at compile time, so only the std tree differs between the two sides).
#
#   bench/std_ab.sh <velt> <base checkout> <head checkout> <program.velt>... [-- <runs>]
#
# Builds each program (--release) against <base>/std and <head>/std, runs each side <runs> times
# (default 2), interleaved, under cachegrind with VELT_THREADS=1 and VELT_RESCUE=0 (one worker,
# so counts repeat), and prints `<program> base=<n> head=<n> change=<percent>` from the medians.
# Programs run with no arguments from their own directory. Needs valgrind and python3.
set -uo pipefail
VELT=$1 BASE=$2 HEAD=$3
shift 3
PROGS=()
RUNS=2
while [ $# -gt 0 ]; do
  if [ "$1" = "--" ]; then RUNS=$2; break; fi
  PROGS+=("$1")
  shift
done
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

count() { # exe: its instructions
  (cd "$(dirname "$1")" && VELT_THREADS=1 VELT_RESCUE=0 valgrind --tool=cachegrind --vgdb=no \
    --cache-sim=no --cachegrind-out-file=/dev/null "$1" 2>&1 >/dev/null) |
    sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,
}

median() {
  python3 -c 'import sys,statistics; v=[int(x) for x in sys.argv[1:] if x]; print(int(statistics.median(v)) if v else "")' "$@"
}

for p in "${PROGS[@]}"; do
  name=$(basename "$p" .velt)
  for side in base head; do
    tree=$BASE
    [ $side = head ] && tree=$HEAD
    (cd "$(dirname "$p")" && VELT_STD="$tree/std" "$VELT" build --release "$(basename "$p")" \
      -o "$WORK/$name-$side") >/dev/null 2>"$WORK/$name-$side.err" ||
      { echo "$name: $side build failed"; head -5 "$WORK/$name-$side.err"; continue 2; }
  done
  b=() h=()
  for _ in $(seq "$RUNS"); do
    b+=("$(count "$WORK/$name-base")")
    h+=("$(count "$WORK/$name-head")")
  done
  mb=$(median "${b[@]}") mh=$(median "${h[@]}")
  change=$(python3 -c 'import sys; b,h=int(sys.argv[1]),int(sys.argv[2]); print(f"{(h-b)*100/b:+.2f}%")' "$mb" "$mh" 2>/dev/null)
  echo "$name base=$mb head=$mh change=$change (runs: ${b[*]} / ${h[*]})"
done
