#!/bin/bash
# The two new goldens, and which calls their release builds make.
cd /Users/andii/dev/velt/branches/alloc-step-a || exit 1
export CARGO_BUILD_JOBS=3 SDKROOT=$(xcrun --sdk macosx --show-sdk-path) VELT_STD=$PWD/std
export CARGO_TARGET_DIR=$PWD/target
L=/private/tmp/claude-501/-Users-andii-dev-velt-main/4c53484a-2d67-40a0-a205-9ca8e43ee06d/scratchpad/alloc-logs
V=$PWD/target/debug/velt
VELT_GOLDEN=lang/array_ cargo test --workspace --test golden > $L/goldens-array.log 2>&1
echo "goldens lang/array_ exit $?: $(grep -E '^test result|stderr missing|exit [0-9]+ \(want' $L/goldens-array.log | head -4 | tr '\n' ' ' | cut -c1-300)"
for g in array_runtime_sizes array_alloc_runtime_past_memory array_alloc_past_memory array_from_past_memory; do
  VELT_HEAP_HOOKS=1 VELT_BUILD_CACHE=off $V build --release --emit vir tests/golden/lang/$g.velt > $L/vir-$g.txt 2>&1
  echo "$g: wrapper alloc $(grep -c 'call.*velt\.heap\.alloc' $L/vir-$g.txt), wrapper realloc $(grep -c 'call.*velt\.heap\.realloc' $L/vir-$g.txt), rt_alloc $(grep -c 'call.*velt_rt_alloc' $L/vir-$g.txt), rt_realloc $(grep -c 'call.*velt_rt_realloc' $L/vir-$g.txt)"
done
