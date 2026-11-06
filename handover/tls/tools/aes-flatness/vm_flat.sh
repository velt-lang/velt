#!/usr/bin/env bash
# Flatness: instructions of flat.vlt per op and input byte, VELT_SIMD_DISABLE=aes, LLVM release
# and Cranelift debug, with the given side's compiler and std (default head).
set -euo pipefail
side=${1:-head}
W=$HOME/aes2417; S=$(dirname "$0")
V=$W/target-$side/release/velt
export VELT_STD=$W/$side/std
cd $W
$V build --release -o $W/flat-$side-rel $S/flat.vlt
$V build -o $W/flat-$side-dbg $S/flat.vlt
for mode in rel dbg; do
  for op in 0 1 2 3 4 5 6 7 8; do
    line="$mode op$op"
    for byte in 00 ff; do
      n=$(OP=$op BYTE=$byte VELT_SIMD_DISABLE=aes valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null $W/flat-$side-$mode 2>&1 >/dev/null | sed -n 's/.*I *refs: *//p' | tr -d ,)
      line="$line $byte=$n"
    done
    echo "$line"
  done
done
