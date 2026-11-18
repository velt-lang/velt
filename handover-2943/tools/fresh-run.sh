#!/bin/bash
# fresh-run.sh <program.velt> [VAR=value ...]: `velt run` of a fresh copy of the program (a
# comment with the time appended: the build cache is keyed by content and would serve an
# earlier build) from std's unit on disk, with the worktree's compiler; extra VAR=value pairs are
# exported first (e.g. VELT_STD=...). Prints the program's output and the build's diagnostics.
W=/Users/andii/dev/velt/branches/d1-open-pr
D=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/fresh-run
mkdir -p "$D/u"
f=$1
shift
for kv in "$@"; do export "${kv?}"; done
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
export VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK="$D/u"
cp "$f" "$D/p.velt"
echo "// fresh $(date +%s%N)" >> "$D/p.velt"
"$W/target/debug/velt" run "$D/p.velt" 2>&1 | grep -v '^velt-units' | head -20
