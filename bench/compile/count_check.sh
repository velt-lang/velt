#!/usr/bin/env bash
# Instructions `velt check bench/compile/all_std.vlt` executes at each given git ref, counted the
# way bench/nightly.sh counts compile/check_all_std (cachegrind, VELT_THREADS=1, release build).
# For bisecting a compile-time change between commits.
#
#   bench/compile/count_check.sh WORKDIR REF...
#
# Each ref is exported to WORKDIR/src-<sha> (git archive) and built with one shared
# CARGO_TARGET_DIR (WORKDIR/target) so consecutive builds reuse what is unchanged. Prints one line
# "<sha>\t<instructions>\t<subject>" per ref. Needs: cargo, valgrind, python3.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WORK=$1; shift
mkdir -p "$WORK"
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
    cargo build --release -q -p veltc --manifest-path "$src/Cargo.toml" >&2
    cp "$CARGO_TARGET_DIR/release/velt" "$bin"
  fi
  # velt check must succeed (as in bench/nightly.sh), or the count measures an error path.
  (cd "$src" && VELT_STD="$src/std" "$bin" check bench/compile/all_std.vlt >/dev/null 2>&1)     || { echo "$sha: velt check bench/compile/all_std.vlt failed" >&2; exit 1; }
  n=$(cd "$src" && VELT_THREADS=1 VELT_STD="$src/std" valgrind --tool=cachegrind --cache-sim=no \
      --cachegrind-out-file="$WORK/cg-$sha.out" "$bin" check bench/compile/all_std.vlt 2>&1 >/dev/null \
      | sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,)
  [ -n "$n" ] || { echo "$sha: cachegrind failed" >&2; exit 1; }
  printf '%s\t%s\t%s\n' "$sha" "$n" "$(git -C "$ROOT" log -1 --format=%s "$sha")"
done
