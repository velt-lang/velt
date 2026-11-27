#!/usr/bin/env bash
# The compiler's instructions (cachegrind, `velt` only: the linker it starts is not followed) for
# `velt check`, `velt build` and `velt build --release` of a program (tests/golden/m1/hello.velt
# by default), by a base and a head `velt`, each with its own std and a fresh build cache of its
# own (a cache's index is read at every start, so a well-used one costs more). Each command runs
# once unmeasured first, so the rows are warm.
#
#   bench/compiler_hello_rows.sh <base velt> <base std> <head velt> <head std> [program.velt]
#
# Needs valgrind on PATH. Prints a Markdown table.
set -euo pipefail

if [ $# -lt 4 ]; then
  sed -n '2,10p' "$0"
  exit 2
fi
base_velt=$1 base_std=$2 head_velt=$3 head_std=$4
root=$(cd "$(dirname "$0")/.." && pwd)
prog=${5:-$root/tests/golden/m1/hello.velt}
work=$(mktemp -d)

# count <side> <velt> <std> args...: the instructions of the second run of `velt args...`.
count() {
  local side=$1 velt=$2 std=$3
  shift 3
  export VELT_BUILD_CACHE="$work/cache-$side" VELT_STD="$std" VELT_THREADS=1
  (cd "$work" && "$velt" "$@" >/dev/null 2>&1)
  (cd "$work" && valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
    "$velt" "$@" 2>&1 >/dev/null) |
    sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,
}

name=$(basename "$prog")
echo "| command | base | head | Δ |"
echo "|---|---:|---:|---:|"
for cmd in check build "build --release"; do
  extra=()
  [ "$cmd" != check ] && extra=(-o "$work/out")
  b=$(count base "$base_velt" "$base_std" $cmd "$prog" "${extra[@]}")
  h=$(count head "$head_velt" "$head_std" $cmd "$prog" "${extra[@]}")
  awk -v n="velt $cmd $name" -v b="$b" -v h="$h" 'BEGIN {
    printf "| `%s` | %d | %d | %+.2f%% |\n", n, b, h, (h - b) * 100 / b
  }'
done
