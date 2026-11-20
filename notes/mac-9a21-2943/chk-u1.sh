#!/bin/bash
# chk-u1.sh <worktree name> <crate>...: cargo check (type-check only) in branches/<worktree>,
# with that worktree's own target.
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/9a2100e7-8c37-4a77-b4d9-1d2c2a1dfa37/scratchpad
cd "/Users/andii/dev/velt/branches/$1" || exit 2
shift
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
unset VELT_BUILD_STAMP VELT_STD CARGO_TARGET_DIR
args=()
for c in "$@"; do args+=(-p "$c"); done
cargo check "${args[@]}" --all-targets > "$S/chk-u1.log" 2>&1
code=$?
grep -E "^(error|warning)" -A14 "$S/chk-u1.log" | head -${CHK_LINES:-120}
grep -cE "^error" "$S/chk-u1.log"
echo "check exit $code"
exit $code
