#!/bin/bash
# #3097 fixes, OrbStack VM (velt-prof): one release velt + velt_rt from the cross-wake worktree into
# $HOME/b3097fix (the stack changes only std, so one compiler serves base and head), and base's std
# (main d205c7827, the stack's base) exported to $HOME/b3097fix/base-std.
#   orb -m velt-prof bash notes/3097/vm-fix/build.sh
set -eu
source "$HOME/.cargo/env"
WT=/Users/andii/dev/velt/branches/cross-wake
OUT=$HOME/b3097fix
mkdir -p "$OUT"
cd "$WT"
CARGO_TARGET_DIR=$OUT/tgt CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 \
  cargo build --release -q -p veltc --bin velt
# Alone: built with veltc, velt_rt's staticlib is not made.
CARGO_TARGET_DIR=$OUT/tgt CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 \
  cargo build --release -q -p velt_rt
rm -rf "$OUT/base-std" && mkdir -p "$OUT/base-std-tmp"
git archive d205c7827 std | tar -x -C "$OUT/base-std-tmp"
mv "$OUT/base-std-tmp/std" "$OUT/base-std" && rmdir "$OUT/base-std-tmp"
ls -la "$OUT/tgt/release/velt" "$OUT/tgt/release/libvelt_rt.a"
