#!/usr/bin/env bash
# A SQLite table-leaf page decoder (#1718's yardstick) in plain TypeScript and in Rust: 64 pages
# of 4096 bytes, 4,580 cells, each cell's two varints and 8 payload bytes read through a
# `subarray` and a `DataView`. Prints instructions per cell for decoding alone (cachegrind, the
# run at 0 rounds subtracted from the run at 400), for Velt, Velt without the per-cell views
# (`pager_noviews.velt`: what escape analysis would give), and Rust.
#   bench/pager/run.sh <velt>
set -euo pipefail
velt=$1
dir=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
"$velt" build --release "$dir/pager.velt" -o "$tmp/velt" >/dev/null
"$velt" build --release "$dir/pager_noviews.velt" -o "$tmp/noviews" >/dev/null
rustc --edition 2021 -C opt-level=3 "$dir/pager.rs" -o "$tmp/rust"
count() {
  VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 |
    sed -n 's/.*I *refs: *//p' | tr -d ','
}
for b in velt noviews rust; do
  z=$(count "$tmp/$b" 0)
  f=$(count "$tmp/$b" 400)
  echo "$b: $(( (f - z) / (400 * 4580) )) instructions per cell"
done
