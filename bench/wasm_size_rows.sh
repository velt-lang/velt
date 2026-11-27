#!/usr/bin/env bash
# The size rows of #3905 step 3 (velt_rt_wasm's own executor goes): for each program, the size
# of its wasm32-wasip1 and wasm32-unknown-unknown modules, built `--release` and debug by a base
# and a head `velt` (each with its own std and its own runtime library), and the size of the
# runtime libraries themselves.
#
#   bench/wasm_size_rows.sh <base velt> <base std> <head velt> <head std> [program.velt ...]
#
# Each `velt` links the runtime library of its own build directory (cargo builds it there for
# both targets first, in the profile of that `velt`). Needs the two wasm Rust targets. Prints
# Markdown tables.
set -euo pipefail

if [ $# -lt 4 ]; then
  sed -n '2,11p' "$0"
  exit 2
fi
base_velt=$1 base_std=$2 head_velt=$3 head_std=$4
shift 4
root=$(cd "$(dirname "$0")/.." && pwd)
if [ $# -eq 0 ]; then
  set -- "$root/tests/golden/m1/hello.velt" "$root/tests/golden/m3/async_basic.velt"
fi
targets=(wasm32-wasip1 wasm32-unknown-unknown)
work=$(mktemp -d)

# The cargo target directory and profile a `velt` was built into (`<target>/<profile>/velt`).
target_dir() { dirname "$(dirname "$1")"; }
profile() { basename "$(dirname "$1")"; }

# runtime <velt> <std> <target>: builds the runtime library next to that `velt`; its path.
runtime() {
  local dir prof flag=()
  dir=$(target_dir "$1") prof=$(profile "$1")
  [ "$prof" = release ] && flag=(--release)
  (cd "$(dirname "$2")" && CARGO_TARGET_DIR=$dir cargo build -q "${flag[@]}" -p velt_rt_wasm \
    --target "$3")
  echo "$dir/$3/$prof/libvelt_rt_wasm.a"
}

# size <velt> <std> <target> <mode flag or empty> <program> <out dir>: the module's size.
size() {
  local out
  out="$6/$3-$(basename "$5" .velt)${4:+-release}.wasm"
  mkdir -p "$6"
  (cd "$6" && VELT_STD="$2" "$1" build $4 --target "$3" "$5" -o "$out" >/dev/null)
  stat -c %s "$out"
}

echo "| program | target | release base | release head | Δ | debug base | debug head | Δ |"
echo "|---|---|---:|---:|---:|---:|---:|---:|"
libs=()
for t in "${targets[@]}"; do
  bl=$(runtime "$base_velt" "$base_std" "$t") hl=$(runtime "$head_velt" "$head_std" "$t")
  libs+=("$t $(stat -c %s "$bl") $(stat -c %s "$hl")")
  for p in "$@"; do
    rb=$(size "$base_velt" "$base_std" "$t" --release "$p" "$work/base")
    rh=$(size "$head_velt" "$head_std" "$t" --release "$p" "$work/head")
    db=$(size "$base_velt" "$base_std" "$t" "" "$p" "$work/base")
    dh=$(size "$head_velt" "$head_std" "$t" "" "$p" "$work/head")
    awk -v n="$(basename "$p")" -v t="$t" -v rb="$rb" -v rh="$rh" -v db="$db" -v dh="$dh" 'BEGIN {
      printf "| %s | %s | %d | %d | %+.1f%% | %d | %d | %+.1f%% |\n", n, t, rb, rh,
        (rh - rb) * 100 / rb, db, dh, (dh - db) * 100 / db
    }'
  done
done
echo
echo "| libvelt_rt_wasm.a | base | head | Δ |"
echo "|---|---:|---:|---:|"
for l in "${libs[@]}"; do
  set -- $l
  awk -v t="$1" -v b="$2" -v h="$3" 'BEGIN { printf "| %s | %d | %d | %+.1f%% |\n", t, b, h, (h - b) * 100 / b }'
done
