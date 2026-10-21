#!/bin/bash
# Instruction counts (cachegrind) of `velt check` for two refs: a sigx file and
# bench/compile/all_std.vlt, both with one thread.
#
# usage: GIT_DIR=<repo> WORK=<dir> SIGX=<sigx checkout> bench/check_instr.sh <base-ref> <head-ref> [<sigx file>]
#   <sigx file> is relative to SIGX (default packages/runtime-core/src/component.ts).
# Each ref is exported into $WORK/<sha> and built there (release) once; both sides read their own
# standard library. Needs valgrind. Prints `<what> <ref> instr=<n>` lines and the change.
set -eu
base=$1
head=$2
file=${3:-packages/runtime-core/src/component.ts}
: "${WORK:?set WORK to a work directory}" "${SIGX:?set SIGX to a sigx checkout}"
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
  echo "$sha"
}

instr() { # <sha> <dir> <file>
  (cd "$2" && VELT_THREADS=1 VELT_STD="$WORK/$1/std" valgrind --tool=cachegrind --cache-sim=no \
    --cachegrind-out-file=/dev/null "$WORK/$1/target/release/velt" check "$3" 2>&1 >/dev/null |
    sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,)
}

b=$(build "$base")
h=$(build "$head")
for what in sigx std; do
  if [ $what = sigx ]; then dir=$SIGX; f=$file; else dir=$WORK/$b; f=bench/compile/all_std.vlt; fi
  nb=$(instr "$b" "$dir" "$f")
  if [ $what = std ]; then dir=$WORK/$h; fi
  nh=$(instr "$h" "$dir" "$f")
  echo "$f base ${b:0:9} instr=$nb"
  echo "$f head ${h:0:9} instr=$nh"
  awk -v a="$nb" -v c="$nh" 'BEGIN { printf "change %+.2f %%\n", (c - a) * 100 / a }'
done
