#!/bin/bash
# #3226 r5 + #3295 r3: build each branch, run its goldens, velt_vir tests, harness.
set -u
export CARGO_TARGET_DIR="$HOME/dev/velt/branches/mac9dce-target"
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk
for b in rest-tuple-infer overload-result; do
  cd "$HOME/dev/velt/branches/$b" || exit 1
  export VELT_STD="$PWD/std"
  echo "######## $b $(git rev-parse --short HEAD)"
  cargo clean -p velt_common -p velt_sema -p velt_vir -p veltc 2>&1 | tail -1
  cargo build -p veltc --bin velt 2>&1 | grep -E "^(error|warning)|Finished" | head -5
  cargo build -p velt_rt 2>&1 | tail -1
  V="$CARGO_TARGET_DIR/debug/velt"
  export VELT_BUILD_CACHE=off VELT_RT_DEBUG_ALLOC=1
  if [ $b = rest-tuple-infer ]; then
    for m in "" --release; do
      echo "=== async golden $m"; "$V" run $m tests/golden/lang/rest_tuple_closure_async.velt 2>&1 | head -8 | cut -c1-200
    done
    echo "=== velt_vir tests"; cargo test -p velt_vir 2>&1 | grep -E "^test result|FAILED|panicked" | head -8 | cut -c1-200
    F="rest_tuple closure"
  else
    echo "=== async error golden"; "$V" check tests/golden/lang/errors/overload_async_callback.velt 2>&1 | sed "s#$PWD/tests/golden/lang/errors/##" | head -8
    F="overload rest_tuple"
  fi
  for f in $F; do
    echo "--- harness $f"
    VELT_GOLDEN=$f cargo test --workspace --test golden 2>&1 | grep -E "^test golden |FAILED|want|got ---|\.velt.*exit|^error" | tail -10 | cut -c1-200
  done
done
