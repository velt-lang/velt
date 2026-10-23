#!/usr/bin/env bash
# Spread arguments and array spreads with pure operands, with calls before a spread (#1458) and
# with a call after a spread of a place (`[...xs, ...f()]`):
# instructions (cachegrind) of each program here, built with every velt given (LLVM release).
#
#   bench/spread_order/run.sh VELT [VELT...]      (Linux; needs valgrind)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${TMPDIR:-/tmp}/velt-spread-order
mkdir -p "$OUT"
for prog in spread_calls spread_late_call spread_late_map; do
  for velt in "$@"; do
    exe="$OUT/$prog-$(echo "$velt" | md5sum | cut -c1-8)"
    "$velt" build --release --backend llvm "$HERE/$prog.vlt" -o "$exe" > /dev/null
    refs=$(VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no \
      --cachegrind-out-file=/dev/null "$exe" 2>&1 >/dev/null | sed -n 's/.*I *refs: *//p')
    echo "$prog	$velt	$refs	$("$exe")"
  done
done
