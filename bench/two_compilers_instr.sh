#!/bin/bash
# Instruction counts (cachegrind, VELT_THREADS=1) of programs built (LLVM release) by two velt
# binaries you already have, with the same standard library and runtime: for a compiler change
# when building both refs in release, as bench/program_instr.sh does, would take too long. The
# generated code does not depend on the profile the compilers were built with. Also the
# instructions of `velt check` on the first program, which do; give both binaries the same profile.
#
# usage: WORK=<dir> VELT_STD=<std> VELT_RT_LIB=<release libvelt_rt.a> \
#   bench/two_compilers_instr.sh <base velt> <head velt> <program.velt>...
# Prints `<program> base=<n> head=<n> change=<%>` lines, and `check <program> ...` first.
set -eu
base=$1
head=$2
shift 2
: "${WORK:?set WORK to a work directory}"
mkdir -p "$WORK"

count() {
  VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null |
    sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,
}

row() { # <name> <base count> <head count>
  awk -v n="$1" -v b="$2" -v h="$3" 'BEGIN { printf "%s base=%d head=%d change=%+.2f%%\n", n, b, h, (h - b) * 100 / b }'
}

row "check $1" "$(count "$base" check "$1")" "$(count "$head" check "$1")"
for p in "$@"; do
  name=$(basename "$p" .velt)
  "$base" build --release --backend llvm "$p" -o "$WORK/$name.base" >/dev/null
  "$head" build --release --backend llvm "$p" -o "$WORK/$name.head" >/dev/null
  row "$p" "$(count "$WORK/$name.base")" "$(count "$WORK/$name.head")"
done
