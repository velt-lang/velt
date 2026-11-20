#!/bin/bash
# #3226 r6: head build + goldens + velt_vir tests + harness; then two break checks of
# rest_tuple_closure_async (r5's ctor/async layout, then r4's), restoring the head after.
set -u
cd "$HOME/dev/velt/branches/rest-tuple-infer" || exit 1
export CARGO_TARGET_DIR="$HOME/dev/velt/branches/mac9dce-target"
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 VELT_STD="$PWD/std"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk
G=tests/golden/lang/rest_tuple_closure_async
V="$CARGO_TARGET_DIR/debug/velt"
echo "######## head $(git rev-parse --short HEAD)"
cargo clean -p velt_common -p velt_sema -p velt_vir -p veltc 2>&1 | tail -1
cargo build -p veltc --bin velt 2>&1 | grep -E "^(error|warning)|Finished" | head -5
cargo build -p velt_rt 2>&1 | tail -1
export VELT_BUILD_CACHE=off VELT_RT_DEBUG_ALLOC=1
for m in "" --release; do
  for th in 1 4; do
    VELT_THREADS=$th "$V" run $m $G.velt > /tmp/r6.out 2>&1
    echo "head $m threads=$th: exit $? $(diff -q /tmp/r6.out $G.out >/dev/null && echo '= Node' || { echo DIFF; diff /tmp/r6.out $G.out | head -6; })"
  done
done
echo "=== velt_vir tests"; cargo test -p velt_vir 2>&1 | grep -E "^test result|FAILED|panicked" | head -6 | cut -c1-200
for f in rest_tuple closure async_fn; do
  echo "--- harness $f"
  VELT_GOLDEN=$f cargo test --workspace --test golden 2>&1 | grep -E "^test golden |FAILED|\.velt.*exit|^error" | tail -6 | cut -c1-200
done
F="crates/velt_vir/src/lower/async_fn/ctor.rs crates/velt_vir/src/lower/async_fn/mod.rs crates/velt_vir/src/lower/rest_tuple_params.rs"
for base in 0c2502e59 2584745e2; do
  echo "######## break: async ctor/layout as at $base"
  for f in $F; do git show "$base:$f" > "$f"; done
  cargo build -p veltc --bin velt 2>&1 | grep -E "^error|Finished" | head -3
  "$V" run $G.velt > /tmp/r6.out 2>&1
  echo "break $base: exit $? $(diff -q /tmp/r6.out $G.out >/dev/null && echo '= Node (TEST DOES NOT BITE)' || echo 'differs (bites)')"
  grep -E "after free|ICE|internal compiler|panic" /tmp/r6.out | head -2 | cut -c1-160
  git checkout -- $F
done
git status --short | head -3
echo "######## rebuild head"
cargo build -p veltc --bin velt 2>&1 | grep -E "^error|Finished" | head -3
