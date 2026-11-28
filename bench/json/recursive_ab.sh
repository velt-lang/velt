#!/usr/bin/env bash
# Instructions of bench/json/recursive.velt (--release, cachegrind) with this tree's std and with
# the std of git ref BASE: what the cycle check of a type that can contain itself costs the
# writer (#3871, #3724). The same compiler builds both programs; only std is swapped. Linux only
# (valgrind).
#
#   bench/json/recursive_ab.sh VELT WORKDIR BASE
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VELT=$1
WORK=$2
BASE=$3
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
rm -rf "$WORK/base-std"
mkdir -p "$WORK/base-std"
git -C "$ROOT" archive "$BASE" std | tar -x -C "$WORK/base-std"
for side in base head; do
  std=$ROOT/std
  [ "$side" = base ] && std=$WORK/base-std/std
  VELT_BUILD_CACHE=off VELT_STD=$std "$VELT" build --release "$ROOT/bench/json/recursive.velt" -o "$WORK/recursive-$side" >/dev/null
  out=$("$WORK/recursive-$side")
  n=$(valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file="$WORK/cg-$side.out" \
      "$WORK/recursive-$side" 2>&1 >/dev/null | sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,)
  printf '%s\t%s\t%s\n' "$side" "$n" "$out"
done
