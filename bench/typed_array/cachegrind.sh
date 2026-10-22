#!/usr/bin/env bash
# Instruction counts (Linux, valgrind) of bench/typed_array/loops.ts's kernels built
# `--release` by the compilers of two git refs, whole process, `VELT_THREADS=1`.
#
#   bench/typed_array/cachegrind.sh BASE_REF HEAD_REF WORK_DIR
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
prog="$W/head/bench/typed_array/loops.ts"
for side in base head; do
  "$W/target-$side/release/velt" build "$prog" -o "$W/loops-$side" --release || exit 1
done
printf "kernel reps base head ratio output\n"
for k in "u8 10" "dot 2" "axpy 5" "i32 5" "xor 50" "nested 20"; do
  set -- $k
  out=()
  irs=()
  for side in base head; do
    o=$(VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
      --log-file="$W/vg-$side.log" "$W/loops-$side" "$1" "$2")
    ir=$(grep -E "I\s+refs" "$W/vg-$side.log" | tr -d ',' | awk '{print $NF}')
    out+=("$o")
    irs+=("$ir")
  done
  same=same
  [ "${out[0]}" = "${out[1]}" ] || same="DIFFERS(${out[0]} / ${out[1]})"
  ratio=$(awk -v a="${irs[0]}" -v b="${irs[1]}" 'BEGIN { printf "%.3f", b / a }')
  echo "$1 $2 ${irs[0]} ${irs[1]} $ratio $same"
done
