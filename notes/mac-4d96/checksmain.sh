#!/bin/bash
# After train 15: in the sleep-home worktree (its target), first #3322's merge with main (detached
# at parked-task), then #3273's (sleep-home, with the owner fix). Logs in checksmain/.
#   bash checksmain.sh [3322|3273|both]
S=$(cd "$(dirname "$0")" && pwd)
L=$S/checksmain; mkdir -p "$L"
what=${1:-both}
cd /Users/andii/dev/velt/branches/sleep-home || exit 1
[ -z "$(git status --porcelain)" ] || { echo "sleep-home is not clean"; exit 1; }
export SDKROOT=$(xcrun --show-sdk-path) CARGO_BUILD_JOBS=3
suites() { # <tag>
  cargo nextest run -p veltc -E 'binary(~std_scheduler) | binary(c_export_async)' --no-fail-fast > "$L/suites-$1.log" 2>&1
  echo "$1 suites exit $?"; grep -E "Summary|^\s+FAIL \[" "$L/suites-$1.log" | sort -u | head -8
}
if [ "$what" != 3273 ]; then
  git checkout -q --detach parked-task || exit 1
  git log --oneline -1
  cargo build --workspace > "$L/build-3322.log" 2>&1; echo "3322 build exit $?"
  VELT_GOLDEN=task_parked cargo test -q -p veltc --test golden > "$L/golden-3322.log" 2>&1; echo "3322 golden exit $? $(grep 'test result' "$L/golden-3322.log" | tail -1)"
  cargo test -q -p veltc --test wasm_goldens > "$L/wasm-3322.log" 2>&1; echo "3322 wasm_goldens exit $? $(grep 'test result' "$L/wasm-3322.log" | tail -1)"
  suites 3322
  cargo xtask precheck > "$L/precheck-3322.log" 2>&1; echo "3322 precheck exit $?"; tail -1 "$L/precheck-3322.log"
  git checkout -q sleep-home
fi
if [ "$what" != 3322 ]; then
  git log --oneline -1
  cargo build --workspace > "$L/build-3273.log" 2>&1; echo "3273 build exit $?"
  cargo test -p veltc --test std_scheduler_sleep_handed > "$L/handed-3273.log" 2>&1; echo "3273 handed exit $? $(grep 'test result' "$L/handed-3273.log" | tail -1)"
  suites 3273
  for g in sleep timer; do VELT_GOLDEN=$g cargo test -q -p veltc --test golden > "$L/golden-3273-$g.log" 2>&1; echo "3273 golden $g exit $? $(grep 'test result' "$L/golden-3273-$g.log" | tail -1)"; done
  cargo xtask precheck > "$L/precheck-3273.log" 2>&1; echo "3273 precheck exit $?"; tail -1 "$L/precheck-3273.log"
fi
git status -sb | head -1
df -g / | tail -1 | awk '{print $4 " GiB free"}'
