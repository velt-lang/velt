#!/usr/bin/env bash
# Instructions (cachegrind) of bench/json/replacer_any_hooked and replacer_any_plain (an `any`
# replacer returning objects with and without a `toJSON` member), built with one release `velt`
# against the std of BASE_REF and against this tree's std: for a change to std/encoding/json
# alone. Linux only (valgrind).
#
#   bench/json/replacer_any_ab.sh VELT BASE_REF WORKDIR
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VELT=$1
REF=$2
WORK=$3
mkdir -p "$WORK/base"
WORK=$(cd "$WORK" && pwd)
git -C "$ROOT" archive "$REF" std | tar -x -C "$WORK/base"
export VELT_BUILD_CACHE=off
for b in replacer_any_hooked replacer_any_plain; do
  for side in base head; do
    std=$ROOT/std
    if [ "$side" = base ]; then
      std=$WORK/base/std
    fi
    VELT_STD=$std "$VELT" build --release "$ROOT/bench/json/$b.velt" -o "$WORK/$b-$side" >/dev/null
    out=$("$WORK/$b-$side")
    n=$(valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
        "$WORK/$b-$side" 2>&1 >/dev/null | sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,)
    printf 'run %s\t%s\t%s\t%s\n' "$b" "$side" "$n" "$out"
  done
done
