#!/bin/bash
# #3261 after merging today's main (5a77956b5): build, module-state suites, precheck.
cd /Users/andii/dev/velt/branches/module-blocks-sync || exit 1
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/76140236-8161-4f16-8c05-0234cd4a600f/scratchpad
export CARGO_BUILD_JOBS=3
export SDKROOT=${SDKROOT:-$(xcrun --show-sdk-path 2>/dev/null)}
export VELT_STD=$PWD/std
git log --oneline -1 | cat
step() {
  local name=$1; shift
  "$@" >"$S/M_$name.log" 2>&1
  local rc=$?
  echo "== $name exit $rc"
  if [ $rc -ne 0 ]; then grep -E "^test .*FAILED|FAILED|panicked at|^error|failures:" -A3 "$S/M_$name.log" | cut -c1-220 | head -20; fi
  return $rc
}
step build cargo build --workspace || exit 1
step goldens_module env VELT_GOLDEN=module_ cargo test --workspace --test golden
step prebuilt_module_blocks cargo test -p veltc --test prebuilt_module_blocks
step prebuilt_weak cargo test -p veltc --test prebuilt_weak
step prebuilt_hooks cargo test -p veltc --test prebuilt_hooks
step module_state_attrs cargo test -p veltc --test module_state_attrs
step module_state_races cargo test -p veltc --test module_state_races
step opt_hot_loops_vir cargo test -p veltc --test opt_hot_loops_vir
step velt_opt cargo test -p velt_opt module_state
step velt_common cargo test -p velt_common
step lib_prebuilt cargo test -p veltc --lib prebuilt
step precheck cargo xtask precheck
git status --short
