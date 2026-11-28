#!/usr/bin/env bash
# Instruction counts (cachegrind) of the rows the math operations touch, debug (Cranelift) and
# --release (LLVM) builds, plus `velt build` of hello (a fresh VELT_HOME each time), for two
# built trees (#2196 track E: the math operations inline, velt_rt's math.rs gone).
#
#   bench/cl_math_ab.sh <label> <tree> <velt binary> [<out.tsv>]
#
# <tree> holds the std and the bench files read (its own std: VELT_STD=<tree>/std). Prints one
# `<label> <row> <mode> instr=<n>` line per row, and appends them to <out.tsv> if given. Run it
# for base and head on the same machine and compare.
set -eu
label=$1
tree=$(cd "$2" && pwd)
velt=$3
out=${4:-/dev/null}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export VELT_STD="$tree/std"

instr() { # <cmd...>: instructions retired by <cmd>, its output thrown away
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null |
    sed -n 's/.*I *refs: *//p' | tr -d ','
}

rows="bench/floats.velt bench/nbody.velt bench/int32.velt bench/stdport/pow.velt
bench/stdport/to_fixed.velt bench/stdport/float_to_string.velt"
for row in $rows; do
  name=$(basename "$row" .velt)
  for mode in debug release; do
    flag=""
    [ "$mode" = release ] && flag="--release"
    "$velt" build $flag "$tree/$row" -o "$work/$name-$mode" >/dev/null 2>&1
    n=$(instr "$work/$name-$mode")
    echo "$label $name $mode instr=$n" | tee -a "$out"
  done
done
for mode in debug release; do
  flag=""
  [ "$mode" = release ] && flag="--release"
  home=$(mktemp -d)
  n=$(VELT_HOME="$home" instr "$velt" build $flag "$tree/tests/golden/m1/hello.velt" -o "$work/hello")
  rm -rf "$home"
  echo "$label hello_build $mode instr=$n" | tee -a "$out"
done
