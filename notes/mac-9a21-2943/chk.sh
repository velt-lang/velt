#!/bin/bash
# chk.sh <crate>...: cargo check of the crates in branches/d1-open-pr; prints errors and warnings.
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/9a2100e7-8c37-4a77-b4d9-1d2c2a1dfa37/scratchpad
cd /Users/andii/dev/velt/branches/d1-open-pr || exit 2
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
unset VELT_BUILD_STAMP VELT_STD
args=()
for c in "$@"; do args+=(-p "$c"); done
cargo check "${args[@]}" --all-targets > "$S/chk.log" 2>&1
code=$?
grep -E "^(error|warning)" -A12 "$S/chk.log" | head -${CHK_LINES:-80}
echo "check exit $code"
exit $code
