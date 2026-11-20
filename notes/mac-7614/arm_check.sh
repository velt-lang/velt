#!/bin/bash
# #3300 arm64 Linux fix: tests, clippy, then three broken versions (each restored from git).
cd /Users/andii/dev/velt/branches/ship-hooks || exit 1
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/76140236-8161-4f16-8c05-0234cd4a600f/scratchpad
export CARGO_BUILD_JOBS=3
export SDKROOT=${SDKROOT:-$(xcrun --show-sdk-path 2>/dev/null)}
git log --oneline -1 | cat
[ -z "$(git status --short)" ] || { echo "tree not clean"; git status --short; exit 1; }
step() {
  local name=$1; shift
  "$@" >"$S/A_$name.log" 2>&1
  local rc=$?
  echo "== $name exit $rc"
  if [ $rc -ne 0 ]; then grep -E "^test .*FAILED|panicked at|^error" -A3 "$S/A_$name.log" | cut -c1-220 | head -16; fi
}
step build cargo build --workspace
step llvm_target cargo test -p velt_codegen_llvm --lib target
step lib_prebuilt cargo test -p veltc --lib prebuilt
step lib_shipped cargo test -p veltc --lib shipped
step prebuilt_hooks cargo test -p veltc --test prebuilt_hooks
step prebuilt_weak cargo test -p veltc --test prebuilt_weak
step install_layout cargo test -p veltc --test install_layout
step clippy cargo clippy -p veltc -p velt_codegen_llvm --all-targets -- -D warnings
grep -h "note: no finder" "$S/A_lib_prebuilt.log" | head -2
P=crates/veltc/src/driver/std_cache/prebuilt.rs
T=crates/velt_codegen_llvm/src/target.rs
mut() { # <name> <file> <test args...>
  local name=$1 file=$2; shift 2
  echo "== mutant $name (applied: $(git diff --stat -- "$file" | tail -1 | grep -c changed))"
  cargo test "$@" 2>&1 | grep -E "^test .*FAILED|^error" | cut -c1-160 | head -6
  git checkout -- "$file"
}
perl -0pi -e 's/    !velt_codegen_llvm::calls_outline_atomics\(target\)\n/    let _ = target;\n    true\n/' "$P"
mut "debug_link_takes always true" "$P" -p veltc --lib prebuilt
perl -0pi -e 's/            false if !debug_link_takes\(&target\) => return None,\n//' "$P"
mut "guard left out of for_build" "$P" -p veltc --lib prebuilt
perl -0pi -e 's/    normalize\(target\)\.is_ok_and\(\|t\| t\.outline_atomics\(\)\)\n/    let _ = target;\n    false\n/' "$T"
mut "calls_outline_atomics always false" "$T" -p velt_codegen_llvm --lib target
git status --short
