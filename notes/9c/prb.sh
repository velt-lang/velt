#!/bin/bash
# 9c PR B in the OrbStack VM (velt-prof): PR B changes the compiler, so two release builds, base =
# PR A (module-lock-velt) and head = PR B (module-blocks-sync), each with its own std. Rows
# (cachegrind, min of 3): `velt build` of hello and of mod_sync.velt (compile cost, one thread),
# and the run of mod_sync.velt and mod_async.velt. Ends by deleting its build dir.
#   orb -m velt-prof bash notes/9c/prb.sh
set -u
source "$HOME/.cargo/env"
OUT=$HOME/b9cb
N=/Users/andii/dev/velt/branches/notes-3097/notes/9c
mkdir -p "$OUT"
for side in base head; do
  wt=/Users/andii/dev/velt/branches/module-lock-velt
  [ $side = head ] && wt=/Users/andii/dev/velt/branches/module-blocks-sync
  (cd "$wt" && CARGO_TARGET_DIR=$OUT/$side CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 \
    cargo build --release -q -p veltc --bin velt &&
    CARGO_TARGET_DIR=$OUT/$side CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 \
    cargo build --release -q -p velt_rt) || { echo "$side: build failed"; exit 1; }
done
refs() {
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null |
    awk '/I *refs:/ {gsub(",", "", $NF); print $NF}'
}
min3() {
  local best=""
  for r in 1 2 3; do
    local v; v=$("$@")
    if [ -z "$best" ] || [ "$v" -lt "$best" ]; then best=$v; fi
  done
  echo "$best"
}
hello=/Users/andii/dev/velt/branches/module-blocks-sync/tests/golden/m1/hello.velt
printf "%-26s %14s %14s %8s\n" row base head delta
row() {
  local name=$1 b=$2 h=$3
  awk -v n="$name" -v b="$b" -v h="$h" 'BEGIN { printf "%-26s %14s %14s %+7.2f%%\n", n, b, h, (h - b) * 100 / b }'
}
build_refs() { # side src out
  local wt=/Users/andii/dev/velt/branches/module-lock-velt
  [ "$1" = head ] && wt=/Users/andii/dev/velt/branches/module-blocks-sync
  rm -f "$3"
  VELT_BUILD_CACHE=off VELT_THREADS=1 VELT_STD=$wt/std VELT_RT_LIB=$OUT/$1/release/libvelt_rt.a \
    refs "$OUT/$1/release/velt" build --release "$2" -o "$3"
}
for prog in "$hello" "$N/mod_sync.velt"; do
  b=$(min3 build_refs base "$prog" "$OUT/p-base")
  h=$(min3 build_refs head "$prog" "$OUT/p-head")
  row "build $(basename "$prog")" "$b" "$h"
done
for prog in "$N/mod_sync.velt" "$N/mod_async.velt"; do
  for side in base head; do
    wt=/Users/andii/dev/velt/branches/module-lock-velt
    [ $side = head ] && wt=/Users/andii/dev/velt/branches/module-blocks-sync
    VELT_BUILD_CACHE=off VELT_STD=$wt/std VELT_RT_LIB=$OUT/$side/release/libvelt_rt.a \
      "$OUT/$side/release/velt" build --release "$prog" -o "$OUT/r-$side" >/dev/null 2>&1
  done
  for t in 1 4; do
    export VELT_THREADS=$t VELT_RESCUE=0
    b=$(min3 refs "$OUT/r-base")
    h=$(min3 refs "$OUT/r-head")
    unset VELT_THREADS VELT_RESCUE
    row "run $(basename "$prog" .velt) T$t" "$b" "$h"
  done
done
rm -rf "$OUT"
df -h "$HOME" | tail -1
