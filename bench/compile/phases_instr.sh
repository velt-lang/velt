#!/usr/bin/env bash
# Instructions a program costs to compile with one toolchain (Linux), in `velt check`, a debug
# build and a release build, by cachegrind with children traced (so clang and the linker count).
# To compare, run it once per side; for a change to std alone, give the same VELT and two STDs.
#
#   bench/compile/phases_instr.sh VELT STD RT_LIB WORK PROGRAM...
#
# RT_LIB is the runtime of VELT's profile (libvelt_rt.a). WORK holds the build outputs and the
# logs. Prints one line per program: `<name> check=<n> debug=<n> release=<n> ir_lines=<n>
# binary=<bytes>`, `ir_lines` being the release LLVM IR's. The build cache and the std cache are
# off, so that each count is the whole compile. Needs: valgrind, clang.
set -euo pipefail
VELT=$(realpath "$1"); STD=$(realpath "$2"); RT=$(realpath "$3"); WORK=$4; shift 4
mkdir -p "$WORK"
WORK=$(realpath "$WORK")
export VELT_STD=$STD VELT_RT_LIB=$RT VELT_BUILD_CACHE=off VELT_THREADS=1

# The sum over every process of `I refs` for `velt <args>`.
count() {
  local logs=$WORK/logs.$$
  mkdir -p "$logs"
  valgrind --tool=cachegrind --cache-sim=no --trace-children=yes --cachegrind-out-file=/dev/null \
    --log-file="$logs/%p.log" "$VELT" "$@" >/dev/null 2>&1 \
    || { echo "failed: velt $*" >&2; exit 1; }
  cat "$logs"/*.log | sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d , \
    | awk '{ s += $1 } END { print s }'
  find "$logs" -name '*.log' -exec unlink {} \;
}

for program in "$@"; do
  name=$(basename "$program" .velt)
  exe=$WORK/$name
  c=$(count check "$program")
  d=$(count build "$program" -o "$exe.debug")
  r=$(count build --release "$program" -o "$exe.release")
  ir=$("$VELT" build --release --emit llvm "$program" | wc -l)
  echo "$name check=$c debug=$d release=$r ir_lines=$ir binary=$(stat -c %s "$exe.release")"
done
