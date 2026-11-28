#!/usr/bin/env bash
# What calls through `any` that throw cost (#1938; Linux, valgrind): instructions (cachegrind,
# VELT_THREADS=1, release compilers) at two git refs of
# - running bench/any_calls/call_loop.velt (600,000 calls through `any` in a `try`, and in a
#   `try` each), bench/any_calls/sort_named.velt (sorts with a named comparator) and
#   bench/any_calls/any_sort.velt (an `any[]` sorted with an arrow that reads through `any`), in a
#   program whose calls through `any` throw, built --release by that ref's compiler;
# - compiling them and tests/golden/lang/any_call_caught.velt (`velt build --release`), and
#   tests/golden/m1/hello.velt, which converts nothing to `any`.
#
#   bench/any_calls/ab.sh WORKDIR BASE_REF HEAD_REF
#
# Each ref is exported to WORKDIR/src-<sha> and built into WORKDIR/target-<sha> (kept: a second run
# reuses them). Both refs compile HEAD_REF's bench files (BENCH=<dir> to take them from a
# directory instead). Every build writes into a fresh directory, so none is skipped as up to date.
# With PROFILE=1 the run rows also write WORKDIR/cg-<sha>-<program>.out for cg_annotate. Keep
# WORKDIR on a Linux file system. Run it from inside the repository, or with GIT_DIR set.
set -uo pipefail
[ $# -ge 3 ] || { echo "usage: bench/any_calls/ab.sh WORKDIR BASE_REF HEAD_REF" >&2; exit 2; }
W=$1
mkdir -p "$W"
W=$(cd "$W" && pwd)
shas=()
for ref in "$2" "$3"; do
  s=$(git rev-parse --short=10 "$ref^{commit}") || exit 1
  shas+=("$s")
  if [ ! -d "$W/src-$s" ]; then
    mkdir -p "$W/src-$s.tmp"
    git archive "$s" | tar -x -C "$W/src-$s.tmp"
    find "$W/src-$s.tmp" -type f -exec touch {} +
    mv "$W/src-$s.tmp" "$W/src-$s"
  fi
  if [ ! -x "$W/target-$s/release/velt" ]; then
    (cd "$W/src-$s" && CARGO_TARGET_DIR="$W/target-$s" cargo build --release -q -p veltc --bin velt \
      && CARGO_TARGET_DIR="$W/target-$s" cargo build --release -q -p velt_rt -p velt_rt_shared) || exit 1
  fi
done
mkdir -p "$W/bench"
for p in call_loop sort_named any_sort; do
  if [ -n "${BENCH:-}" ]; then
    cp "$BENCH/$p.velt" "$W/bench/$p.velt" || exit 1
  else
    git show "${shas[1]}:bench/any_calls/$p.velt" > "$W/bench/$p.velt" || exit 1
  fi
done
git show "${shas[1]}:tests/golden/lang/any_call_caught.velt" > "$W/bench/any_call_caught.velt" || exit 1
# count <sha> <name> <program and args...>: the instructions of one run in that ref's tree.
count() {
  local s=$1 name=$2 out=/dev/null
  shift 2
  [ -n "${PROFILE:-}" ] && out="$W/cg-$s-$name.out"
  (cd "$W/src-$s" && VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_STD="$W/src-$s/std" valgrind \
    --tool=cachegrind --cache-sim=no --cachegrind-out-file="$out" "$@" 2>&1 >/dev/null |
    grep -oP 'I\s+refs:\s+\K[\d,]+' | tr -d ,)
}
for s in "${shas[@]}"; do
  v="$W/target-$s/release/velt"
  b=$(mktemp -d "$W/bin-$s.XXXXXX")
  for p in call_loop sort_named any_sort; do
    (cd "$W/src-$s" && VELT_STD="$W/src-$s/std" VELT_BUILD_CACHE=off "$v" build --release \
      "$W/bench/$p.velt" -o "$b/$p") || exit 1
    echo "$s run $p $(count "$s" "$p" "$b/$p")"
    "$b/$p"
  done
  for p in call_loop sort_named any_sort any_call_caught; do
    echo "$s build $p $(count "$s" "build-$p" "$v" build --release "$W/bench/$p.velt" -o "$b/c-$p")"
  done
  echo "$s build hello $(count "$s" build-hello "$v" build --release tests/golden/m1/hello.velt -o "$b/h")"
  rm -rf "$b"
done
