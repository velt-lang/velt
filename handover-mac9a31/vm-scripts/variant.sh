#!/bin/bash
# Inside the VM: lines.velt built by the head compiler against the std copy named by $1
# (a directory under branches/vm-stdin), then cachegrind's instruction count reading the 1M lines.
set -u
V=$VELT_ROOT/branches/vm-stdin
OUT=$HOME/stdin-out
export VELT_BUILD_CACHE=off VELT_THREADS=1
VELT_STD=$V/$1 VELT_RT_LIB=$HOME/tgt-stdin-head/release/libvelt_rt.a \
  "$HOME/tgt-stdin-head/release/velt" build "$V/lines.velt" --release -o "$OUT/lines-$1" || exit 1
echo "$1 output: $("$OUT/lines-$1" < "$OUT/lines.txt")"
valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
  "$OUT/lines-$1" < "$OUT/lines.txt" > "$OUT/$1.log" 2>&1
echo "$1 $(grep -o 'I *refs: *[0-9,]*' "$OUT/$1.log" | tr -dc '0-9')"
