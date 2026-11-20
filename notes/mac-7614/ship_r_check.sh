#!/bin/bash
# #3300 review round 1: the tests, then each new check broken once (restored after).
cd /Users/andii/dev/velt/branches/ship-hooks || exit 1
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/76140236-8161-4f16-8c05-0234cd4a600f/scratchpad
export CARGO_BUILD_JOBS=3
export SDKROOT=${SDKROOT:-$(xcrun --show-sdk-path 2>/dev/null)}
step() {
  local name=$1; shift
  "$@" >"$S/R_$name.log" 2>&1
  local rc=$?
  echo "== $name exit $rc"
  if [ $rc -ne 0 ]; then grep -E "^test .*FAILED|panicked at|^error" -A3 "$S/R_$name.log" | cut -c1-220 | head -16; fi
}
step lib cargo test -p veltc --lib shipped
step lib_prebuilt cargo test -p veltc --lib prebuilt
step lib_cli cargo test -p veltc --lib cli
step install_layout cargo test -p veltc --test install_layout
step prebuilt_hooks cargo test -p veltc --test prebuilt_hooks
step clippy cargo clippy -p veltc --all-targets -- -D warnings
SH=crates/veltc/src/driver/std_cache/shipped.rs
cp "$SH" "$S/keep_shipped2.rs"
mut() {
  echo "== mutant $1 (applied: $2)"
  cargo test -p veltc --lib shipped 2>&1 | grep -E "^test .*FAILED" | cut -c1-160 | head -6
  cp "$S/keep_shipped2.rs" "$SH"
}
perl -0pi -e 's/    if vpm::probe::content_hash\(&bytes\) != manifest\.object \{\n        return None;\n    \}\n//' "$SH"
mut "no object hash check" "$(grep -c 'content_hash(&bytes) != manifest.object' "$SH" | tr 01 yn)"
perl -0pi -e 's/        let text = text\.strip_suffix\(&format!\("\\n\{END\}\\n"\)\)\?;\n/        let text = text.strip_suffix(\&format!("\\n{END}\\n")).unwrap_or(text);\n/' "$SH"
mut "end marker not required" "$(grep -c 'unwrap_or(text)' "$SH")"
perl -0pi -e 's/        if files\.is_empty\(\) \{\n            return None;\n        \}\n//' "$SH"
mut "no file lines accepted" "$(grep -c 'if files.is_empty()' "$SH" | tr 01 yn)"
git status --short
