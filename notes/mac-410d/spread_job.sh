#!/bin/bash
# #943 spread: build spread-param, run its goldens and the neighbours through the harness, the
# error golden's output, the break check, the sigx kit.
set -u
S=${S:-$HOME/dev/velt/branches/handovers/mac-410d-128/out}  # logs and outputs
Q=$HOME/dev/velt/branches/handovers/mac-410d-128  # sx copy + kitdiff.py
mkdir -p "$S/out"
cd "$HOME/dev/velt/branches/spread-param" || exit 1
export CARGO_TARGET_DIR="$HOME/dev/velt/branches/mac9dce-target"
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 VELT_STD="$PWD/std"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk
cargo clean -p velt_common -p velt_sema -p velt_vir -p veltc 2>&1 | tail -1
cargo build -p veltc --bin velt 2>&1 | grep -E "^(error|warning)|Finished" | head -5
cargo build -p velt_rt 2>&1 | tail -1
V="$CARGO_TARGET_DIR/debug/velt"
export VELT_BUILD_CACHE=off
for g in spread_type_param_rest overload_rest_result; do
  echo "=== run $g"
  VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 "$V" run tests/golden/lang/$g.velt >"$S/out/$g.out" 2>"$S/out/$g.err"
  echo "exit $? $(diff -q "$S/out/$g.out" tests/golden/lang/$g.out >/dev/null && echo 'stdout = Node' || echo DIFF)"
  head -8 "$S/out/$g.err" | cut -c1-220
  diff "$S/out/$g.out" tests/golden/lang/$g.out | head -10
done
echo "=== error golden"
echo "--- refused golden"
"$V" check tests/golden/lang/errors/spread_type_param_refused.velt 2>&1 | sed "s#$PWD/tests/golden/lang/errors/##" | head -20
echo "--- NoLock trace"
VELT_MUT=2 "$V" check tests/golden/lang/errors/spread_type_param_lock.velt 2>&1 | grep "^NoLock" | head -6 | cut -c1-200
"$V" check tests/golden/lang/errors/spread_type_param_lock.velt 2>&1 | sed "s#$PWD/tests/golden/lang/errors/##" | head -12
"$V" build tests/golden/lang/errors/spread_type_param_lock.velt -o "$S/out/lockbin" 2>&1 | sed "s#$PWD/tests/golden/lang/errors/##" | head -12
echo "=== break check (tuple expansion off)"
for g in spread_type_param_rest overload_rest_result; do
  echo "$g: $(VELT_MUT=1 "$V" run tests/golden/lang/$g.velt 2>&1 | head -3 | tr '\n' '|' | cut -c1-200)"
done
echo "=== harness"
for f in spread overload rest_tuple; do
  echo "--- $f"
  VELT_GOLDEN=$f cargo test --workspace --test golden 2>&1 | grep -E "^test golden |FAILED|want|got ---|\.velt.*exit|^error" | tail -8 | cut -c1-200
done
echo "=== kit"
( cd "$Q/sx" && python3 "$HOME/dev/sigx/resume-kit/kit.py" "$V" "$Q/sx" "$S/kit-spread.json" >"$S/kit-spread.log" 2>&1 )
tail -1 "$S/kit-spread.log"
python3 -I "$Q/kitdiff.py" "$Q/kit-b2a.json" "$S/kit-spread.json" | grep -v "head.ts left"
