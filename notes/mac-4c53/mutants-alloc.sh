#!/bin/bash
# Mutants of std's heap hooks, each in a fresh copy of std (never the worktree's): the new
# goldens must fail on each. Output to a log; never pipe this script into head.
W=/Users/andii/dev/velt/branches/alloc-step-a
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/4c53484a-2d67-40a0-a205-9ca8e43ee06d/scratchpad
L=$S/alloc-logs
cd $W || exit 1
export CARGO_BUILD_JOBS=3 SDKROOT=$(xcrun --sdk macosx --show-sdk-path) CARGO_TARGET_DIR=$W/target
run() { # name, python replacement script args: old new
  local name=$1 old=$2 new=$3
  rm -rf $S/mstd && cp -R $W/std $S/mstd
  python3 $S/replace_once.py $S/mstd/lang/hooks/heap.velt "$old" "$new" || { echo "$name: pattern not found"; return; }
  VELT_STD=$S/mstd VELT_GOLDEN=lang/array_ cargo test --workspace --test golden > $L/mutant-$name.log 2>&1
  local s=$?
  echo "$name: golden exit $s ($( [ $s = 0 ] && echo SURVIVED || echo killed)): $(grep -E 'stderr missing|exit [0-9-]+ \(want|want ---' $L/mutant-$name.log | head -2 | tr '\n' ' ' | cut -c1-200)"
}
run no-oom-check "  if (p == 0) {
    outOfMemory(size);
  }
  return p;" "  return p;"
run realloc-ignores-empty "  if (old == 0) {
    return heapAlloc(size, align);
  }" ""
run alloc-zero-null "  if (size == 0) {
    return align;
  }" "  if (size == 0) {
    return 0;
  }"
rm -rf $S/mstd
cd $W && git status --porcelain std | head -3; echo "worktree std clean check done"
