#!/usr/bin/env bash
# Instructions per phase of phases.vlt under cachegrind, the fill subtracted, for a standard
# library against another with one compiler (a std-only change):
#
#   bench/concurrent_map/phases.sh <velt> <base std dir> <head std dir> [kinds...]
#
# Kinds default to `u64` (both std dirs) and `str strtie map` (the head's only). Prints one line
# per kind and phase: base, head and the difference, in instructions. Needs valgrind and clang.
set -euo pipefail
VELT=$1
BASE_STD=$2
HEAD_STD=$3
shift 3
KINDS=("$@")
if [ ${#KINDS[@]} -eq 0 ]; then
  KINDS=(u64 str strtie map)
fi
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${OUT:-$(mktemp -d)}
VELT_STD=$BASE_STD "$VELT" build --release --backend llvm "$HERE/phases.vlt" -o "$OUT/base"
VELT_STD=$HEAD_STD "$VELT" build --release --backend llvm "$HERE/phases.vlt" -o "$OUT/head"

instr() {
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null |
    sed -n 's/.*I *refs: *//p' | tr -d ,
}

for kind in "${KINDS[@]}"; do
  phases=(hit miss set delete)
  sides=(head)
  if [ "$kind" = u64 ]; then
    phases=(hit miss set add delete many)
    sides=(base head)
  fi
  declare -A fill=()
  for side in "${sides[@]}"; do
    fill[$side]=$(instr "$OUT/$side" "$kind" fill)
  done
  echo "$kind fill ${fill[*]}"
  for phase in "${phases[@]}"; do
    line="$kind $phase"
    for side in "${sides[@]}"; do
      line="$line $(($(instr "$OUT/$side" "$kind" "$phase") - fill[$side]))"
    done
    echo "$line"
  done
  unset fill
done
