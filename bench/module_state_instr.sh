#!/bin/bash
# Instruction counts (cachegrind) of two refs for programs with module state in a root file with
# top-level statements (bench/proxy/proxy_signal.vlt, proxy_tick_proxiable.vlt), built with
# `--release`, and of `velt check bench/compile/all_std.vlt` (one thread).
#
# usage: GIT_DIR=<repo> WORK=<dir> bench/module_state_instr.sh <base-ref> <head-ref>
# Each ref is exported into $WORK/<sha> and built there (release) once. Needs valgrind.
set -eu
: "${WORK:?set WORK to a work directory}"
mkdir -p "$WORK"

build() {
  local sha
  sha=$(git rev-parse "$1^{commit}")
  if [ ! -x "$WORK/$sha/target/release/velt" ]; then
    mkdir -p "$WORK/$sha"
    git archive "$sha" | tar -x -C "$WORK/$sha"
    (cd "$WORK/$sha" && CARGO_TARGET_DIR="$WORK/$sha/target" cargo build --release -q -p veltc --bin velt &&
      CARGO_TARGET_DIR="$WORK/$sha/target" cargo build --release -q -p velt_rt)
  fi
  echo "$sha"
}

refs() { # <command...>: instruction count
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null |
    sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,
}

for ref in "$1" "$2"; do
  sha=$(build "$ref")
  dir=$WORK/$sha
  for p in proxy/proxy_signal proxy/proxy_tick_proxiable; do
    exe=$dir/$(basename $p)
    (cd "$dir" && VELT_RT_LIB="$dir/target/release/libvelt_rt.a" "$dir/target/release/velt" build \
      --release "bench/$p.vlt" -o "$exe" >/dev/null)
    echo "$ref $(basename $p) instr=$(refs "$exe")"
  done
  echo "$ref check_all_std instr=$(cd "$dir" && VELT_THREADS=1 refs "$dir/target/release/velt" check bench/compile/all_std.vlt)"
done
