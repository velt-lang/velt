#!/usr/bin/env bash
# Maps whose value type includes null, read while they hold tombstones (#1005): instructions
# (cachegrind) of each program here, built with every velt given (LLVM release). `bulk_*` read
# with keys(), values() and entries(); `live_*` with forEach and for...of. `plain` is
# Map<string, number>, `nullable` Map<string, number | null> with no null value, `half_null`
# the same with every other value null.
#
#   bench/map_null/run.sh VELT [VELT...]      (Linux; needs valgrind)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${TMPDIR:-/tmp}/velt-map-null
mkdir -p "$OUT"
for prog in bulk_plain bulk_nullable bulk_half_null live_plain live_nullable live_half_null; do
  for velt in "$@"; do
    exe="$OUT/$prog-$(echo "$velt" | md5sum | cut -c1-8)"
    "$velt" build --release --backend llvm "$HERE/$prog.vlt" -o "$exe" > /dev/null
    refs=$(VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no \
      --cachegrind-out-file=/dev/null "$exe" 2>&1 >/dev/null | sed -n 's/.*I *refs: *//p')
    echo "$prog	$velt	$refs	$("$exe")"
  done
done
