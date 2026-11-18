#!/bin/bash
# #2196 9c PR A, OrbStack VM (velt-prof): one release velt + velt_rt from the module-lock-velt
# worktree into $HOME/b9c (PR A changes only std, so one compiler serves base and head), and base's
# std (PR A's merge base) exported to $HOME/b9c/base-std.
#   orb -m velt-prof bash notes/9c/build.sh
set -eu
source "$HOME/.cargo/env"
WT=/Users/andii/dev/velt/branches/module-lock-velt
OUT=$HOME/b9c
BASE=${BASE:-047e190172}
mkdir -p "$OUT"
cd "$WT"
CARGO_TARGET_DIR=$OUT/tgt CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 \
  cargo build --release -q -p veltc --bin velt
# Alone: built with veltc, velt_rt's staticlib is not made.
CARGO_TARGET_DIR=$OUT/tgt CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 \
  cargo build --release -q -p velt_rt
rm -rf "$OUT/base-std" "$OUT/base-std-tmp" && mkdir -p "$OUT/base-std-tmp"
git archive "$BASE" std | tar -x -C "$OUT/base-std-tmp"
mv "$OUT/base-std-tmp/std" "$OUT/base-std" && rmdir "$OUT/base-std-tmp"
ls -la "$OUT/tgt/release/velt" "$OUT/tgt/release/libvelt_rt.a"
