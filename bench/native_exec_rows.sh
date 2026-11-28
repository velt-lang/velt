#!/usr/bin/env bash
# Native instructions of #3905 step 2's std changes (the executor's root and end shared with a JS
# host's turns, its start-up's runtime calls behind `#sched-thread`): for each program, the
# instructions its `--release` native build takes (cachegrind; the program alone, velt not
# counted), built by a base and a head `velt`, each with its own std.
#
#   bench/native_exec_rows.sh <base velt> <base std> <head velt> <head std> [program.velt ...]
#
# Needs valgrind on PATH. Prints a Markdown table.
set -euo pipefail

if [ $# -lt 4 ]; then
  sed -n '2,9p' "$0"
  exit 2
fi
base_velt=$1 base_std=$2 head_velt=$3 head_std=$4
shift 4
root=$(cd "$(dirname "$0")/.." && pwd)
if [ $# -eq 0 ]; then
  set -- "$root/tests/golden/m3/async_basic.velt" "$root/bench/async/promise_settle.velt" \
    "$root/bench/async/fanout_all.velt" "$root/bench/async/race_small.velt"
fi
work=$(mktemp -d)

# build <velt> <std> <program> <out dir>: the executable's path.
build() {
  local out="$4/$(basename "$3" .velt)"
  mkdir -p "$4"
  (cd "$root" && VELT_STD="$2" "$1" build --release "$3" -o "$out" >/dev/null)
  echo "$out"
}

# instructions <executable>: the instructions of its run.
instructions() {
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file="$1.cg" "$1" >/dev/null \
    2>"$1.log" || { echo "$1 failed: $1.log" >&2; echo fails; return; }
  grep -E "I +refs:" "$1.log" | tr -d ',' | awk '{print $NF}'
}

echo "| program | instructions base | instructions head | Δ |"
echo "|---|---:|---:|---:|"
for p in "$@"; do
  b=$(build "$base_velt" "$base_std" "$p" "$work/base")
  h=$(build "$head_velt" "$head_std" "$p" "$work/head")
  bi=$(instructions "$b") hi=$(instructions "$h")
  awk -v n="$(basename "$p")" -v bi="$bi" -v hi="$hi" 'BEGIN {
    d = (bi == "fails" || hi == "fails") ? "" : sprintf("%+.2f%%", (hi - bi) * 100 / bi)
    printf "| %s | %s | %s | %s |\n", n, bi, hi, d
  }'
done
