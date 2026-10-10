#!/bin/sh
# Run time of programs under the checking allocator (VELT_RT_DEBUG_ALLOC=1) with two debug
# runtimes, e.g. a copy of <target>/debug/velt_rt.lib built from the base and one from the branch.
# Each program is built once per runtime (debug), then the two binaries run alternately <reps>
# times; prints the total wall milliseconds per program and runtime.
# Usage: bench/debug_alloc_runs.sh <base-runtime-lib> <branch-runtime-lib> <reps> <program.vlt>...
base=${1:?usage: debug_alloc_runs.sh <base-lib> <branch-lib> <reps> <program>...}
branch=${2:?}
reps=${3:?}
shift 3
work=${TMP:-/tmp}/debug-alloc-runs
mkdir -p "$work"
ms() { date +%s%3N; }
for prog in "$@"; do
  name=$(basename "$prog" .vlt)
  VELT_RT_LIB=$base cargo run -q -p veltc --bin velt -- build "$prog" -o "$work/$name-base.exe" || exit 1
  VELT_RT_LIB=$branch cargo run -q -p veltc --bin velt -- build "$prog" -o "$work/$name-branch.exe" || exit 1
  tb=0
  tr=0
  i=0
  while [ $i -lt "$reps" ]; do
    s=$(ms); VELT_RT_DEBUG_ALLOC=1 "$work/$name-base.exe" > /dev/null || exit 1; e=$(ms); tb=$((tb + e - s))
    s=$(ms); VELT_RT_DEBUG_ALLOC=1 "$work/$name-branch.exe" > /dev/null || exit 1; e=$(ms); tr=$((tr + e - s))
    i=$((i + 1))
  done
  echo "$name: base ${tb} ms, branch ${tr} ms ($reps runs each)"
done
