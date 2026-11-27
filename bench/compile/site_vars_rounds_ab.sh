#!/usr/bin/env bash
# What two compilers cost compiling programs with promises, and how many sema rounds they take
# with error-set variables and without them (#3893): instructions of `velt build --release`
# (cachegrind, VELT_THREADS=1, velt only: the linker is not followed), and the
# `velt-sema:   round:` lines of `velt build --emit vir` with VELT_SEMA_TIMINGS=1, with the
# variables and with VELT_NO_SITE_VARS=1 (as eager_std_identity's site_vars test compares them).
# Linux only (valgrind). Run it from the repository's root; programs are read from there.
#
#   bench/compile/site_vars_rounds_ab.sh BASE_VELT BASE_STD HEAD_VELT HEAD_STD [PROGRAM...]
#
# Without programs: the two goldens of #3893, site_vars_identity's issue_3893 programs and
# m1/hello. Prints one row per program: base and head instructions, the change, and the rounds
# "base/head (without variables base/head)".
set -uo pipefail
[ $# -ge 4 ] || {
  echo "usage: bench/compile/site_vars_rounds_ab.sh BASE_VELT BASE_STD HEAD_VELT HEAD_STD [PROGRAM...]" >&2
  exit 2
}
vb=$1 sb=$2 vh=$3 sh=$4
shift 4
progs=("$@")
if [ ${#progs[@]} -eq 0 ]; then
  progs=(tests/golden/lang/promise_text.velt tests/golden/std/http_server/request_body_read_twice.velt
    crates/veltc/tests/site_vars_identity/issue_3893_*.velt tests/golden/m1/hello.velt)
fi
out=$(mktemp -d)
count() {
  VELT_THREADS=1 VELT_BUILD_CACHE=off valgrind --tool=cachegrind --cache-sim=no \
    --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null | grep -oP 'I\s+refs:\s+\K[\d,]+' | tr -d ,
}
rounds() {
  VELT_BUILD_CACHE=off VELT_SEMA_TIMINGS=1 "$@" 2>&1 >/dev/null | grep -c 'velt-sema:   round:'
}
printf '%-70s %14s %14s %8s  %s\n' program base head change "rounds base/head (without variables)"
for p in "${progs[@]}"; do
  n=$(basename "$p" .velt)
  cb=$(VELT_STD=$sb count "$vb" build --release "$p" -o "$out/b-$n")
  ch=$(VELT_STD=$sh count "$vh" build --release "$p" -o "$out/h-$n")
  rb=$(VELT_STD=$sb rounds "$vb" build "$p" --emit vir)
  rh=$(VELT_STD=$sh rounds "$vh" build "$p" --emit vir)
  nb=$(VELT_NO_SITE_VARS=1 VELT_STD=$sb rounds "$vb" build "$p" --emit vir)
  nh=$(VELT_NO_SITE_VARS=1 VELT_STD=$sh rounds "$vh" build "$p" --emit vir)
  pct=$(awk -v a="$cb" -v b="$ch" 'BEGIN { if (a > 0) printf "%+.2f%%", (b - a) * 100 / a; else print "?" }')
  printf '%-70s %14s %14s %8s  %s/%s (%s/%s)\n' "$p" "$cb" "$ch" "$pct" "$rb" "$rh" "$nb" "$nh"
done
rm -rf "$out"
