#!/usr/bin/env bash
# The rows of #3905 step 2 (a JS host's programs on the executor in Velt): for each program, the
# size of its `--release` wasm32-unknown-unknown module and the wall time of its run under node
# through the JS glue (best of N runs, base and head interleaved), built by a base and a head
# `velt`, each with its own std and glue. Wall time is context only: node's runs are not counted
# in instructions (its JIT and the glue run too).
#
#   bench/wasm_js_rows.sh <base velt> <base std> <base glue> <head velt> <head std> <head glue> [program.velt ...]
#
# A glue is crates/velt_rt_wasm/js/velt_web.mjs of that tree. Needs node and the
# wasm32-unknown-unknown Rust target. RUNS (default 5) sets N. Prints a Markdown table.
set -euo pipefail

if [ $# -lt 6 ]; then
  sed -n '2,11p' "$0"
  exit 2
fi
base_velt=$1 base_std=$2 base_glue=$3 head_velt=$4 head_std=$5 head_glue=$6
shift 6
runs=${RUNS:-5}
root=$(cd "$(dirname "$0")/.." && pwd)
if [ $# -eq 0 ]; then
  set -- "$root/tests/golden/m1/hello.velt" "$root/tests/golden/m3/async_basic.velt" \
    "$root/bench/async/promise_settle.velt" "$root/bench/async/fanout_all.velt" \
    "$root/bench/async/race_small.velt" "$root/bench/async/promise_identity_loop.velt"
fi
work=$(mktemp -d)

# build <velt> <std> <program> <out dir>: the module's path.
build() {
  local out="$4/$(basename "$3" .velt).wasm"
  mkdir -p "$4"
  (cd "$root" && VELT_STD="$2" "$1" build --release --target wasm32-unknown-unknown "$3" -o "$out" >/dev/null)
  echo "$out"
}

# ms <glue> <module>: the wall time of one run in milliseconds ("fails" when it fails).
ms() {
  local start end
  start=$(date +%s%N)
  node "$1" "$2" >/dev/null 2>&1 || { echo fails; return; }
  end=$(date +%s%N)
  echo $(((end - start) / 1000000))
}

echo "| program | size base | size head | Δ | ms base | ms head |"
echo "|---|---:|---:|---:|---:|---:|"
for p in "$@"; do
  b=$(build "$base_velt" "$base_std" "$p" "$work/base")
  h=$(build "$head_velt" "$head_std" "$p" "$work/head")
  bs=$(stat -c %s "$b") hs=$(stat -c %s "$h")
  bt=999999999 ht=999999999
  for _ in $(seq "$runs"); do
    t=$(ms "$base_glue" "$b")
    [ "$t" = fails ] && bt=fails
    [ "$bt" != fails ] && [ "$t" -lt "$bt" ] && bt=$t
    t=$(ms "$head_glue" "$h")
    [ "$t" = fails ] && ht=fails
    [ "$ht" != fails ] && [ "$t" -lt "$ht" ] && ht=$t
  done
  awk -v n="$(basename "$p")" -v bs="$bs" -v hs="$hs" -v bt="$bt" -v ht="$ht" 'BEGIN {
    printf "| %s | %d | %d | %+.1f%% | %s | %s |\n", n, bs, hs, (hs - bs) * 100 / bs, bt, ht
  }'
done
