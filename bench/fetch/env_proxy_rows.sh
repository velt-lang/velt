#!/bin/bash
# Instruction counts (cachegrind) for the proxy of fetch (#3693): `velt check` and `velt build`
# of a program without fetch (tests/golden/m1/hello.velt; the build counts velt alone, not clang
# or the linker), and bench/fetch/loopback.velt built --release and run without a proxy.
#
# usage: bench/fetch/env_proxy_rows.sh <base velt> <base std> <head velt> <head std> <work dir>
#   For a std-only change pass the same velt twice and swap only the std directories.
# Runs from the repository root; needs valgrind. Prints `<row> base=<n> head=<n> <change>`.
set -eu
base_velt=$1 base_std=$2 head_velt=$3 head_std=$4 work=$5
mkdir -p "$work"
# No proxy variables: the rows measure a program that does not use a proxy.
unset NODE_USE_ENV_PROXY http_proxy HTTP_PROXY https_proxy HTTPS_PROXY no_proxy NO_PROXY
export VELT_THREADS=1

instr() { # <program> <args...>: instructions of that process alone
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null |
    sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,
}

row() { # <name> <base count> <head count>
  awk -v n="$1" -v a="$2" -v c="$3" 'BEGIN { printf "%s base=%d head=%d %+.2f%%\n", n, a, c, (c - a) * 100 / a }'
}

hello=tests/golden/m1/hello.velt
for side in base head; do
  velt=$base_velt std=$base_std
  [ $side = head ] && velt=$head_velt std=$head_std
  export VELT_STD=$std
  eval "${side}_check=\$(instr \"\$velt\" check \"\$hello\")"
  eval "${side}_build=\$(instr \"\$velt\" build -o \"\$work/hello_$side\" \"\$hello\")"
  (cd bench/fetch && "$velt" build --release -o "$work/loopback_$side" loopback.velt >/dev/null)
done
row "hello check" "$base_check" "$head_check"
row "hello build (velt only)" "$base_build" "$head_build"
for run in 1 2 3; do
  b=$(instr "$work/loopback_base")
  h=$(instr "$work/loopback_head")
  row "fetch loopback --release run $run" "$b" "$h"
done
