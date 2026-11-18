#!/bin/bash
# vm-build.sh: (in the OrbStack VM velt-prof) build this branch's velt and runtime into the VM's
# home (never the worktree's target/), for cachegrind profiles. Remove $HOME/tgt-7de7 after.
source "$HOME/.cargo/env"
cd /Users/andii/dev/velt/branches/d1-open-pr || exit 1
export CARGO_TARGET_DIR=$HOME/tgt-7de7 CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
cargo build -q -p veltc --bin velt && cargo build -q -p velt_rt
echo "vm build $?"
ls -la "$CARGO_TARGET_DIR/debug/velt" "$CARGO_TARGET_DIR/debug/libvelt_rt.a" 2>&1 | awk '{print $5, $9}'
