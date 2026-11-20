#!/bin/bash
# veltc's units tests against #3301's std, from alloc-step-a's debug build: first with main's
# units.rs (expected to fail: the Error.toString program), then with #3301's new one. Restores
# alloc-step-a's units.rs at the end.
A=/Users/andii/dev/velt/branches/alloc-step-a
F=/Users/andii/dev/velt/branches/fs-nul-3260
L=/private/tmp/claude-501/-Users-andii-dev-velt-main/4c53484a-2d67-40a0-a205-9ca8e43ee06d/scratchpad/alloc-logs
cd $A || exit 1
export CARGO_BUILD_JOBS=3 SDKROOT=$(xcrun --sdk macosx --show-sdk-path) CARGO_TARGET_DIR=$A/target
VELT_STD=$F/std cargo test -p veltc --test units > $L/units-old.log 2>&1
echo "old program, #3301 std: exit $? $(grep -E '^test result|FAILED|panicked' $L/units-old.log | head -3 | tr '\n' ' ' | cut -c1-250)"
cp $F/crates/veltc/tests/units.rs crates/veltc/tests/units.rs
VELT_STD=$F/std cargo test -p veltc --test units > $L/units-new.log 2>&1
echo "new program, #3301 std: exit $? $(grep -E '^test result|FAILED|panicked' $L/units-new.log | head -3 | tr '\n' ' ' | cut -c1-250)"
cargo test -p veltc --test units > $L/units-new-main.log 2>&1
echo "new program, main std: exit $? $(grep -E '^test result|FAILED|panicked' $L/units-new-main.log | head -3 | tr '\n' ' ' | cut -c1-250)"
git checkout -- crates/veltc/tests/units.rs
git status --porcelain | head -3
