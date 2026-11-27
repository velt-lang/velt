#!/usr/bin/env bash
# Instructions (cachegrind) of the rows of a change to `JSON.stringify` and its replacer walk, at
# BASE and at this tree (this tree's programs on both sides): each side its own release `velt`
# and std, no build cache. Run rows: bench/json/replacer, stringify and recursive (--release);
# compile rows: the compiler process of `velt build --release` for tests/golden/m1/hello.velt, an
# app of bench/compile/scale/app.py 10000, and bench/json/replacer.velt. Linux only (valgrind).
#
#   bench/json/rows_ab.sh BASE_VELT BASE_TREE HEAD_VELT WORKDIR
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
BASE_VELT=$1
BASE_TREE=$(cd "$2" && pwd)
HEAD_VELT=$3
WORK=$4
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
export VELT_BUILD_CACHE=off
refs() {
  sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,
}
[ -d "$WORK/app10k" ] || python3 "$ROOT/bench/compile/scale/app.py" 10000 "$WORK/app10k" >/dev/null
for side in base head; do
  velt=$HEAD_VELT
  tree=$ROOT
  if [ "$side" = base ]; then
    velt=$BASE_VELT
    tree=$BASE_TREE
  fi
  for b in replacer stringify recursive; do
    VELT_STD=$tree/std "$velt" build --release "$ROOT/bench/json/$b.velt" -o "$WORK/$b-$side" >/dev/null
    out=$("$WORK/$b-$side" | head -c 200)
    n=$(valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file="$WORK/cg-$b-$side.out" \
        "$WORK/$b-$side" 2>&1 >/dev/null | refs)
    printf 'run %s\t%s\t%s\t%s\n' "$b" "$side" "$n" "$out"
  done
  for c in "$ROOT/tests/golden/m1/hello.velt" "$WORK/app10k/main.velt" "$ROOT/bench/json/replacer.velt"; do
    n=$(VELT_STD=$tree/std valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
        "$velt" build --release "$c" -o "$WORK/c-$side" 2>&1 >/dev/null | refs | head -1)
    printf 'compile %s\t%s\t%s\n' "$(basename "$(dirname "$c")")/$(basename "$c")" "$side" "$n"
  done
done
