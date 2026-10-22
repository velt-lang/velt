#!/usr/bin/env bash
# Cost of an access through a proxy (Linux, valgrind): instruction counts of
# bench/proxy/proxy_get.ts built by the compilers of two git refs, for N reads and writes
# through `get` / `set` traps and for N plain reads, at two sizes (the difference per 100000
# iterations is the cost of the loop body, without start-up), and of bench/proxy/proxy_signal.vlt
# (a sigx-style reactive state behind a typed proxy).
#
#   bench/proxy/cachegrind.sh BASE_REF HEAD_REF WORK_DIR
#
# Run it from inside the repository, or with GIT_DIR set.
set -uo pipefail
BASE=$1
HEAD=$2
W=$3
mkdir -p "$W"
for side in base head; do
  ref=$BASE
  [ "$side" = head ] && ref=$HEAD
  if [ ! -d "$W/$side" ]; then
    mkdir -p "$W/$side"
    git archive "$ref" | tar -x -C "$W/$side"
  fi
  (cd "$W/$side" && CARGO_TARGET_DIR="$W/target-$side" cargo build --release -q -p veltc -p velt_rt) || exit 1
done
prog="$W/head/bench/proxy/proxy_get.ts"
for side in base head; do
  "$W/target-$side/release/velt" build "$prog" -o "$W/proxy_get-$side" --release || exit 1
  for mode in proxy plain; do
    for n in 100000 200000; do
      ir=$(VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
        "$W/proxy_get-$side" "$n" "$mode" 2>&1 >/dev/null | grep -E "I\s+refs" | tr -d ',' |
        awk '{print $NF}')
      echo "$side $mode $n $ir"
    done
  done
  "$W/target-$side/release/velt" build "$W/head/bench/proxy/proxy_signal.vlt" -o "$W/proxy_signal-$side" --release || exit 1
  ir=$(VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$W/proxy_signal-$side" 2>&1 >/dev/null | grep -E "I\s+refs" | tr -d ',' | awk '{print $NF}')
  echo "$side proxy_signal - $ir"
done
