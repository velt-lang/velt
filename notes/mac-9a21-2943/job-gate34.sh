#!/bin/bash
# job-gate34.sh: precheck and scripts/check.sh at #3336's head (units-disk, which holds #3335).
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/9a2100e7-8c37-4a77-b4d9-1d2c2a1dfa37/scratchpad
W=/Users/andii/dev/velt/branches/d1-open-pr
L=$S/gate34
mkdir -p "$L"
cd "$W" || exit 2
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
unset VELT_BUILD_STAMP VELT_STD VELT_UNITS VELT_UNITS_SPLIT VELT_UNITS_BUILD VELT_UNITS_DISK
git diff --quiet || { echo "worktree not clean"; exit 2; }
back=$(git symbolic-ref --short HEAD) || exit 2
git fetch -q origin units-disk
git checkout -q --detach FETCH_HEAD || exit 2
echo "== units-disk $(git rev-parse --short HEAD)"
cargo xtask precheck > "$L/precheck.log" 2>&1
pre=$?
echo "precheck exit $pre"
if [ $pre -eq 0 ]; then
  scripts/check.sh > "$L/gate.log" 2>&1
  echo "gate exit $?"
  tail -17 "$L/gate.log" | cut -c1-120
else
  tail -8 "$L/precheck.log" | cut -c1-200
fi
git checkout -q "$back" && echo "back on $back"
