#!/bin/bash
# Instruction counts (cachegrind, VELT_THREADS=1) of programs built (LLVM release) by two refs.
#
# usage: GIT_DIR=<repo> WORK=<dir> bench/program_instr.sh <base-ref> <head-ref> <program.vlt>...
# Each ref is exported into $WORK/<sha> and its compiler built there once; every program (a path
# in the head ref, read from the head tree) is built by both. Needs valgrind. Prints
# `<program> base=<n> head=<n> change=<%>` lines.
set -eu
base=$1
head=$2
shift 2
: "${WORK:?set WORK to a work directory}"
mkdir -p "$WORK"

build() {
  local sha
  sha=$(git rev-parse "$1^{commit}")
  if [ ! -x "$WORK/$sha/target/release/velt" ]; then
    rm -rf "${WORK:?}/$sha"
    mkdir -p "$WORK/$sha"
    git archive "$sha" | tar -x -C "$WORK/$sha"
    (cd "$WORK/$sha" && CARGO_TARGET_DIR="$WORK/$sha/target" cargo build --release -q -p veltc --bin velt)
  fi
  if [ ! -f "$WORK/$sha/target/release/libvelt_rt.a" ]; then
    (cd "$WORK/$sha" && CARGO_TARGET_DIR="$WORK/$sha/target" cargo build --release -q -p velt_rt)
  fi
  echo "$sha"
}

instr() { # <sha> <program>
  local exe
  exe="$WORK/$1/$(basename "$2" .vlt)"
  (cd "$WORK/$1" && VELT_STD="$WORK/$1/std" target/release/velt build --release --backend llvm \
    "$WORK/$h/$2" -o "$exe" >/dev/null)
  VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
    "$exe" 2>&1 >/dev/null | sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,
}

b=$(build "$base")
h=$(build "$head")
for p in "$@"; do
  nb=$(instr "$b" "$p")
  nh=$(instr "$h" "$p")
  echo "$p base=$nb head=$nh change=$(awk -v b="$nb" -v h="$nh" 'BEGIN { printf "%+.2f%%", (h - b) * 100 / b }')"
done
