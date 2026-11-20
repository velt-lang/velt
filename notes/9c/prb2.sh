#!/bin/bash
# 9c PR B with module_blocks.velt prebuilt (crew #125), in the OrbStack VM (velt-prof). Two release
# compilers, each with its own std: base = PR A on #3153 (worktree module-blocks-base), head = PR B
# (worktree module-blocks-sync, branch module-blocks-prebuilt). Rows (cachegrind, min of 3):
#   warm: `velt build --release` of hello and mod_sync.velt with a build cache `velt cache warm`
#         filled (a fresh copy of it per build, so no build serves the next one);
#   cold: the same with the build cache off (the module is loaded: the pre-warm window);
#   run:  mod_sync.velt and mod_async.velt as the warm builds made them.
# Ends by deleting its build dir.
#   orb -m velt-prof bash notes/9c/prb2.sh
set -u
source "$HOME/.cargo/env"
OUT=$HOME/b125
N=/Users/andii/dev/velt/branches/notes-3097/notes/9c
B=/Users/andii/dev/velt/branches
mkdir -p "$OUT"
wt() { [ "$1" = head ] && echo "$B/module-blocks-sync" || echo "$B/module-blocks-base"; }
for side in base head; do
  (cd "$(wt $side)" && CARGO_TARGET_DIR=$OUT/$side CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 \
    cargo build --release -q -p veltc --bin velt &&
    CARGO_TARGET_DIR=$OUT/$side CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 \
    cargo build --release -q -p velt_rt) || { echo "$side: build failed"; exit 1; }
  # The warm cache of this side.
  VELT_BUILD_CACHE=$OUT/warm-$side VELT_STD=$(wt $side)/std VELT_RT_LIB=$OUT/$side/release/libvelt_rt.a \
    VELT_CACHE_WARM=on "$OUT/$side/release/velt" cache warm >"$OUT/warm-$side.log" 2>&1 ||
    { echo "$side: cache warm failed"; tail -5 "$OUT/warm-$side.log"; }
  tail -2 "$OUT/warm-$side.log"
  du -sh "$OUT/warm-$side" | cut -f1
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
hello=$B/module-blocks-sync/tests/golden/m1/hello.velt
printf "%-28s %14s %14s %8s\n" row base head delta
row() {
  awk -v n="$1" -v b="$2" -v h="$3" 'BEGIN { printf "%-28s %14s %14s %+7.2f%%\n", n, b, h, (h - b) * 100 / b }'
}
build_refs() { # side cache(warm|off) src out
  local cache=off
  if [ "$2" = warm ]; then
    rm -rf "$OUT/run-cache" && cp -a "$OUT/warm-$1" "$OUT/run-cache"
    cache=$OUT/run-cache
  fi
  rm -f "$4"
  VELT_BUILD_CACHE=$cache VELT_BUILD_STAMP=off VELT_THREADS=1 VELT_STD=$(wt $1)/std \
    VELT_RT_LIB=$OUT/$1/release/libvelt_rt.a refs "$OUT/$1/release/velt" build --release "$3" -o "$4"
}
for cache in warm off; do
  for prog in "$hello" "$N/mod_sync.velt"; do
    b=$(min3 build_refs base $cache "$prog" "$OUT/p-base")
    h=$(min3 build_refs head $cache "$prog" "$OUT/p-head")
    row "build $cache $(basename "$prog" .velt)" "$b" "$h"
  done
done
for prog in "$N/mod_sync.velt" "$N/mod_async.velt" "$N/mod_async_tight.velt" "$N/mod_sync_tight.velt"; do
  for side in base head; do
    rm -rf "$OUT/run-cache" && cp -a "$OUT/warm-$side" "$OUT/run-cache"
    VELT_BUILD_CACHE=$OUT/run-cache VELT_BUILD_STAMP=off VELT_STD_OBJECT_STATS=1 VELT_STD=$(wt $side)/std \
      VELT_RT_LIB=$OUT/$side/release/libvelt_rt.a \
      "$OUT/$side/release/velt" build --release "$prog" -o "$OUT/r-$side" >"$OUT/r-$side.log" 2>&1 ||
      { echo "$side: build of $prog failed"; tail -5 "$OUT/r-$side.log"; }
    grep -h "module_blocks\|objects linked" "$OUT/r-$side.log" | sed "s/^/  $side $(basename "$prog" .velt): /"
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
