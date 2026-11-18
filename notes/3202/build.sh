#!/bin/bash
# #3202 review round, OrbStack VM (velt-prof): one release velt + velt_rt from main 8e95c69c2 with
# #3173 (26ed97111) applied (worktree vm-base-3202), as the reviewer's builder compiler; base std
# = main 8e95c69c2's, head std = the cross-wake worktree's (std-only stack).
#   orb -m velt-prof bash notes/3202/build.sh
set -eu
source "$HOME/.cargo/env"
WT=/Users/andii/dev/velt/branches/vm-base-3202
OUT=$HOME/b3202
mkdir -p "$OUT"
cd "$WT"
CARGO_TARGET_DIR=$OUT/tgt CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 \
  cargo build --release -q -p veltc --bin velt
CARGO_TARGET_DIR=$OUT/tgt CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 \
  cargo build --release -q -p velt_rt
rm -rf "$OUT/base-std" && cp -R "$WT/std" "$OUT/base-std"
ls -la "$OUT/tgt/release/velt" "$OUT/tgt/release/libvelt_rt.a"
