#!/usr/bin/env bash
# Instructions per b-tree cell decoded from SQLite-like pages in a `Uint8Array` (_pages.vlt):
# through a `DataView` (dataview.vlt), a `subarray` per page with shifts (subarray.vlt), the whole
# `Uint8Array` with shifts (flat.vlt), and Velt's `u8[]` with integer types (bytes.vlt, the
# baseline), against Rust's `&[u8]` with `u16::from_be_bytes`/`u32::from_be_bytes` (rust/).
# Counts user-space instructions, which a loaded machine does not change: the difference between
# R2 and R1 rounds of decoding, divided by the cells decoded.
#
#   bench/typed_array/page_cells/run.sh [R1] [R2]
#
# Needs: cargo, python3; clang for the LLVM backend. On macOS `/usr/bin/time -l` counts the
# instructions, on Linux `valgrind --tool=cachegrind`.
set -euo pipefail
R1=${1:-10}
R2=${2:-110}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  python3 -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-page-cells"
mkdir -p "$OUT"
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
for v in dataview subarray flat bytes; do
  "$TARGET/release/velt" build --release --backend llvm "$HERE/$v.vlt" -o "$OUT/$v"
done
(cd "$HERE/rust" && cargo build --release -q --target-dir "$OUT/rust")
instr() { # <exe> <rounds>
  if [ "$(uname)" = Darwin ]; then
    /usr/bin/time -l "$1" "$2" 2>&1 >/dev/null | awk '/instructions retired/ {print $1}'
  else
    valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$1" "$2" 2>&1 |
      awk '/I *refs:/ {gsub(",", "", $NF); print $NF}'
  fi
}
per_cell() { # <exe>
  local cells a b
  cells=$("$1" 1 | awk '{print $1}')
  a=$(instr "$1" "$R1")
  b=$(instr "$1" "$R2")
  python3 -c "print(f'{($b - $a) / (($R2 - $R1) * $cells):.1f}')"
}
echo "| program | instructions per cell |"
echo "|---|---:|"
for v in dataview subarray flat bytes; do
  echo "| Velt $v.vlt | $(per_cell "$OUT/$v") |"
done
echo "| Rust \`&[u8]\` | $(per_cell "$OUT/rust/release/cells_bench") |"
