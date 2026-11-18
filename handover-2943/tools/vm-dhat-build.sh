#!/bin/bash
# vm-dhat-build.sh: (in the VM) a velt with the system allocator, for DHAT: a clone of the
# worktree's branch in the VM's home ($HOME/src-7de7, never the worktree), with the worktree's
# uncommitted changes copied in, and veltc's velt_rt_host taken without mimalloc; built into
# $HOME/tgt-dhat. Remove both after (vm-cleanup.sh).
source "$HOME/.cargo/env"
W=/Users/andii/dev/velt/branches/d1-open-pr
C=$HOME/src-7de7
rm -rf "$C"
git clone -q --no-hardlinks "$W" "$C" || exit 1
git -C "$C" checkout -q "$(git -C "$W" rev-parse HEAD)" || exit 1
for f in $(git -C "$W" diff --name-only HEAD); do cp "$W/$f" "$C/$f"; done
sed -i 's|^velt_rt_host = { path = "../velt_rt_host" }$|velt_rt_host = { path = "../velt_rt_host", default-features = false }|' "$C/crates/veltc/Cargo.toml"
grep -n "velt_rt_host" "$C/crates/veltc/Cargo.toml"
# veltc reads its peak memory from mimalloc directly: without it, no peak (0) in --timings.
sed -i '/^    unsafe { mi_process_info(null, null, null, null, &mut peak, null, null, null) };$/d; /^    let null = std::ptr::null_mut();$/d; s/^    let mut peak = 0usize;$/    let peak = 0usize;/' "$C/crates/veltc/src/driver/memory.rs"
grep -n "mi_process_info" "$C/crates/veltc/src/driver/memory.rs"
cd "$C" || exit 1
export CARGO_TARGET_DIR=$HOME/tgt-dhat CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
cargo build -q -p veltc --bin velt && cargo build -q -p velt_rt
echo "dhat build $?"
