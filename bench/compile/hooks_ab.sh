#!/usr/bin/env bash
# Compile and run cost of two compilers, each with its own std, by instructions (cachegrind,
# VELT_THREADS=1, the velt process only: the linker and clang are not counted), for a change to
# how lowering finds std's hooks (#2197): `velt check` and a debug build of each program, a fresh
# empty VELT_HOME and the build cache off per run, then the run of each program's release build.
#
#   bench/compile/hooks_ab.sh <base velt> <base std> <head velt> <head std> <work dir> <program>...
#
# A program is a .velt file or a directory (its main.velt). Prints
# `<row> <program> base=<n> head=<n> change=<%>` lines. Needs valgrind.
set -euo pipefail
base=$1 base_std=$2 head=$3 head_std=$4 work=$5
shift 5
mkdir -p "$work"

count() { # <std> <command...>: the instructions of the command, its stdout dropped
  local std=$1 home
  shift
  home=$(mktemp -d "$work/home.XXXXXX")
  VELT_STD=$std VELT_HOME=$home VELT_BUILD_CACHE=off VELT_THREADS=1 \
    valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null |
    sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,
}

row() { # <name> <base count> <head count>
  awk -v n="$1" -v b="$2" -v h="$3" \
    'BEGIN { printf "%s base=%d head=%d change=%+.2f%%\n", n, b, h, (h - b) * 100 / b }'
}

for p in "$@"; do
  src=$p
  [ -d "$p" ] && src=$p/main.velt
  name=$(basename "$p" .velt)
  row "check $name" "$(count "$base_std" "$base" check "$src")" \
    "$(count "$head_std" "$head" check "$src")"
  row "debug $name" "$(count "$base_std" "$base" build "$src" -o "$work/$name.dbase")" \
    "$(count "$head_std" "$head" build "$src" -o "$work/$name.dhead")"
  VELT_STD=$base_std "$base" build --release "$src" -o "$work/$name.base" >/dev/null
  VELT_STD=$head_std "$head" build --release "$src" -o "$work/$name.head" >/dev/null
  row "run $name" "$(count "$base_std" "$work/$name.base")" "$(count "$head_std" "$work/$name.head")"
done
