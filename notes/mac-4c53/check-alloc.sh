#!/bin/bash
# Allocator step A checks: build, unit tests, heap_hooks test, goldens, clippy. Logs to $L.
cd /Users/andii/dev/velt/branches/alloc-step-a || exit 1
export CARGO_BUILD_JOBS=3 SDKROOT=$(xcrun --sdk macosx --show-sdk-path) VELT_STD=$PWD/std
export CARGO_TARGET_DIR=$PWD/target
L=/private/tmp/claude-501/-Users-andii-dev-velt-main/4c53484a-2d67-40a0-a205-9ca8e43ee06d/scratchpad/alloc-logs
mkdir -p $L
echo "== alloc-step-a @ $(git rev-parse --short HEAD), $(git status --porcelain | wc -l | tr -d ' ') changed"
step() {
  local name=$1; shift
  "$@" > $L/$name.log 2>&1
  local s=$?
  echo "$name exit $s: $(grep -E '^test result|error(\[|:)|FAILED|panicked' $L/$name.log | tail -2 | tr '\n' ' ' | cut -c1-200)"
  return $s
}
step build cargo build --workspace || exit 1
step opt-unit cargo test -p velt_opt --lib heap_calls
step llvm-unit cargo test -p velt_codegen_llvm --lib
step hooks cargo test -p veltc --test heap_hooks
step common cargo test -p velt_common
# Every release golden now allocates through the wrappers: all of them.
step goldens cargo test --workspace --test golden
step clippy cargo clippy -p velt_opt -p velt_codegen_llvm -p veltc -p velt_vir --all-targets -- -D warnings
step veltfmt cargo run -q -p veltc --bin velt -- fmt --check std
echo "df: $(df -g / | tail -1 | awk '{print $4}') GiB free"
# #3314 round 1 (fs-win-write-3309 @ 1a3096575): this build's velt over that worktree's std.
F=/Users/andii/dev/velt/branches/fs-win-write-3309
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/4c53484a-2d67-40a0-a205-9ca8e43ee06d/scratchpad
V=$PWD/target/debug/velt
(cd $F && VELT_STD=$F/std $V fmt --check std/fs > $L/3314-fmt.log 2>&1); echo "3314 fmt exit $?"
(cd $F && VELT_STD=$F/std $V check tests/golden/std/fs_win_write_trailing.velt > $L/3314-parse.log 2>&1); echo "3314 golden check exit $? (Windows-only imports may differ)"
cd "$S/run" || exit 1
for t in x86_64-pc-windows-msvc aarch64-pc-windows-msvc wasm32-wasip1; do
  rm -f r4-$t.obj
  VELT_STD=$F/std $V build wincheck.velt --target $t --emit obj -o r4-$t.obj > "$L/3314-cross-$t.log" 2>&1
  [ -f r4-$t.obj ] && echo "3314 cross $t ok" || echo "3314 cross $t FAILED"
done
# Not precheck there: it would rebuild that branch's crates into this target. Its std part:
(cd $F && VELT_STD=$F/std $V run scripts/std-lint/main.velt > $L/3314-stdlint.log 2>&1); echo "3314 std-lint exit $?: $(tail -1 $L/3314-stdlint.log | cut -c1-120)"
