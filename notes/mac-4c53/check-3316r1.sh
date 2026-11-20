#!/bin/bash
# #3316 round 1: heap_hooks on the worktree's std, then with arrayTooLong's panic put back in a
# copy of std (must fail). Light: only the test crate rebuilds. Logs to $L.
W=/Users/andii/dev/velt/branches/alloc-step-a
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/4c53484a-2d67-40a0-a205-9ca8e43ee06d/scratchpad
L=$S/alloc-logs
cd $W || exit 1
export CARGO_BUILD_JOBS=3 SDKROOT=$(xcrun --sdk macosx --show-sdk-path) CARGO_TARGET_DIR=$W/target
unset VELT_STD
cargo test -p veltc --test heap_hooks > $L/r1-hooks.log 2>&1
echo "hooks exit $?: $(grep -E '^test result' $L/r1-hooks.log | tail -1)"
rm -rf $S/mstd && cp -R $W/std $S/mstd
python3 $S/replace_once.py $S/mstd/lang/hooks/heap.velt '  velt_rt_flush();
  velt_rt_write_str(
    2,
    "panic: RangeError: Invalid array length: an array holds at most 2^52 - 1 elements, and its elements at most 2^52 - 1 bytes\n",
  );
  velt_rt_exit(101);' '  panic(
    "RangeError: Invalid array length: an array holds at most 2^52 - 1 elements, and its elements at most 2^52 - 1 bytes",
  );' || echo "MUTANT PATTERN NOT FOUND"
VELT_STD=$S/mstd cargo test -p veltc --test heap_hooks past_the_limit > $L/r1-mutant.log 2>&1
s=$?
echo "panic put back: exit $s ($( [ $s = 0 ] && echo SURVIVED || echo killed)): $(grep -E 'panicked at|caught_past|grow_past_limit|empty_past' $L/r1-mutant.log | head -3 | tr '\n' ' ' | cut -c1-300)"
rm -rf $S/mstd
git status --porcelain std
V=$W/target/debug/velt
VELT_STD=$W/std $V fmt --check std/lang/hooks/heap.velt && echo "veltfmt ok"
cargo fmt --all -- --check > /dev/null 2>&1 && echo "cargo fmt ok" || echo "cargo fmt DIFF"
cargo clippy -p veltc --tests -- -D warnings > $L/r1-clippy.log 2>&1; echo "clippy veltc exit $?"
cargo test -p velt_opt --lib heap_calls > $L/r1-opt.log 2>&1; echo "opt exit $?: $(grep -E '^test result' $L/r1-opt.log | tail -1)"
