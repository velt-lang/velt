#!/usr/bin/env bash
# Where timer_await's extra instructions at T=4 go: cachegrind + cg_annotate (top functions) for
# the base and head builds left by run-s12.sh in ~/s12.
set -uo pipefail
OUT=$HOME/s12
for s in base head; do
  VELT_THREADS=4 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=$OUT/ta-$s.cg $OUT/timer_await-$s >/dev/null 2>&1
  echo "== $s"
  cg_annotate $OUT/ta-$s.cg 2>/dev/null | grep -E '^ *[0-9,]+ +\(' | head -25
done
