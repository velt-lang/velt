#!/usr/bin/env bash
# The rows of #3905 step 1 (the executor in Velt on wasm32-wasip1): for each program, the size of
# its `--release` wasm32-wasip1 module and the instructions its run takes under wasmtime
# (cachegrind, the module compiled ahead with `wasmtime compile`, so Cranelift's compile is not
# counted), built by a base and a head `velt`, each with its own std.
#
#   bench/wasm_exec_rows.sh <base velt> <base std> <head velt> <head std> [program.velt ...]
#
# Needs wasmtime and valgrind on PATH, and the wasm32-wasip1 Rust target. Prints a Markdown table.
set -euo pipefail

if [ $# -lt 4 ]; then
  sed -n '2,9p' "$0"
  exit 2
fi
base_velt=$1 base_std=$2 head_velt=$3 head_std=$4
shift 4
root=$(cd "$(dirname "$0")/.." && pwd)
if [ $# -eq 0 ]; then
  set -- "$root/tests/golden/m1/hello.velt" "$root/tests/golden/m3/async_basic.velt" \
    "$root/bench/async/promise_settle.velt" "$root/bench/async/fanout_all.velt" \
    "$root/bench/async/race_small.velt" "$root/bench/async/promise_identity_loop.velt" \
    "$root/bench/async/timers.velt"
fi
work=$(mktemp -d)

# build <velt> <std> <program> <out dir>: the module's path.
build() {
  local out="$4/$(basename "$3" .velt).wasm"
  mkdir -p "$4"
  (cd "$root" && VELT_STD="$2" "$1" build --release --target wasm32-wasip1 "$3" -o "$out" >/dev/null)
  echo "$out"
}

# instructions <module>: the instructions of its run under wasmtime.
instructions() {
  local cwasm="${1%.wasm}.cwasm"
  # For the baseline x86-64: valgrind's CPU lacks some of the host's extensions.
  wasmtime compile --target x86_64-unknown-linux-gnu "$1" -o "$cwasm"
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file="$cwasm.cg" \
    wasmtime run --allow-precompiled --dir . "$cwasm" >/dev/null 2>"$cwasm.log" ||
    { echo "$1 failed: $cwasm.log" >&2; echo fails; return; }
  grep -E "I +refs:" "$cwasm.log" | tr -d ',' | awk '{print $NF}'
}

echo "| program | size base | size head | Δ | instructions base | instructions head | Δ |"
echo "|---|---:|---:|---:|---:|---:|---:|"
for p in "$@"; do
  b=$(build "$base_velt" "$base_std" "$p" "$work/base")
  h=$(build "$head_velt" "$head_std" "$p" "$work/head")
  bs=$(stat -c %s "$b") hs=$(stat -c %s "$h")
  bi=$(cd "$work" && instructions "$b") hi=$(cd "$work" && instructions "$h")
  awk -v n="$(basename "$p")" -v bs="$bs" -v hs="$hs" -v bi="$bi" -v hi="$hi" 'BEGIN {
    d = (bi == "fails" || hi == "fails") ? "" : sprintf("%+.1f%%", (hi - bi) * 100 / bi)
    printf "| %s | %d | %d | %+.1f%% | %s | %s | %s |\n", n, bs, hs, (hs - bs) * 100 / bs,
      bi, hi, d
  }'
done
