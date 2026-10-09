#!/usr/bin/env bash
# Instructions `velt check bench/compile/all_std.vlt` executes at each given git ref, counted the
# way bench/nightly.sh counts compile/check_all_std (cachegrind, VELT_THREADS=1, release build).
# For bisecting a compile-time change between commits. Linux only (valgrind).
#
#   bench/compile/count_check.sh [--prepend LINE] WORKDIR REF...
#
# --prepend LINE checks all_std.vlt with LINE added at the top (written next to it as
# all_std_variant.vlt), to measure what one more import costs at a ref, e.g.
# --prepend 'import * as m from "velt:url";'.
#
# Each ref is exported to WORKDIR/src-<sha> (git archive) and built there, with that tree's
# toolchain and cargo config, into one shared CARGO_TARGET_DIR (WORKDIR/target) so consecutive
# builds reuse what is unchanged. Prints one line "<sha>\t<instructions>\t<subject>" per ref.
# Needs: git, tar, cargo, valgrind.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
PREPEND=
if [ "${1:-}" = --prepend ]; then PREPEND=$2; shift 2; fi
[ $# -ge 2 ] || { echo "usage: bench/compile/count_check.sh [--prepend LINE] WORKDIR REF..." >&2; exit 2; }
WORK=$1; shift
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
export CARGO_TARGET_DIR="$WORK/target"
for ref in "$@"; do
  sha=$(git -C "$ROOT" rev-parse --short=8 "$ref^{commit}")
  src="$WORK/src-$sha"
  if [ ! -d "$src" ]; then
    mkdir -p "$src.tmp"
    git -C "$ROOT" archive "$sha" | tar -x -C "$src.tmp"
    # git archive stamps files with the commit time, older than the outputs in the shared target
    # dir: without a fresh mtime cargo would reuse the previous ref's build.
    find "$src.tmp" -type f -exec touch {} +
    mv "$src.tmp" "$src"
  fi
  bin="$WORK/velt-$sha"
  if [ ! -x "$bin" ]; then
    (cd "$src" && cargo build --release -q -p veltc) >&2
    cp "$CARGO_TARGET_DIR/release/velt" "$bin"
  fi
  input=bench/compile/all_std.vlt
  if [ -n "$PREPEND" ]; then
    input=bench/compile/all_std_variant.vlt
    { printf '%s\n' "$PREPEND"; cat "$src/bench/compile/all_std.vlt"; } > "$src/$input"
  fi
  # velt check must succeed (as in bench/nightly.sh), or the count measures an error path.
  (cd "$src" && VELT_STD="$src/std" "$bin" check "$input" >/dev/null 2>&1) \
    || { echo "$sha: velt check $input failed" >&2; exit 1; }
  n=$(cd "$src" && VELT_THREADS=1 VELT_STD="$src/std" valgrind --tool=cachegrind --cache-sim=no \
      --cachegrind-out-file="$WORK/cg-$sha${PREPEND:+-variant}.out" "$bin" check "$input" 2>&1 >/dev/null \
      | sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,)
  [ -n "$n" ] || { echo "$sha: cachegrind failed" >&2; exit 1; }
  printf '%s\t%s\t%s\n' "$sha" "$n" "$(git -C "$ROOT" log -1 --format=%s "$sha")"
done
