#!/bin/bash
# ab-base.sh: build velt at b98c5957b (the commit before mac-7de7's two) in the d1-open-pr
# worktree, keep it as target/debug/velt-b98, go back to units-passes and rebuild head; then
# ab7.sh for both binaries.
R=/Users/andii/dev/velt/branches/d1-open-pr
T=/Users/andii/dev/velt/branches/handovers/mac-e688-2943/tools
cd "$R" || exit 1
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
cp target/debug/velt target/debug/velt-head
git checkout -q b98c5957b || exit 1
cargo build -q -p veltc --bin velt || { git checkout -q units-passes; exit 1; }
cp target/debug/velt target/debug/velt-b98
git checkout -q units-passes || exit 1
cargo build -q -p veltc --bin velt || exit 1
cmp -s target/debug/velt target/debug/velt-head && echo "head binary rebuilt identically" || echo "head binary differs from the copy (expected: build ids)"
echo "== base b98c5957b"
bash "$T/ab7.sh" "$R/target/debug/velt-b98"
echo "== head $(git rev-parse --short HEAD)"
bash "$T/ab7.sh" "$R/target/debug/velt"
