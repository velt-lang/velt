#!/bin/bash
# job-u34.sh, in branches/d1-open-pr's target: U3c (u3c-pr) and U4 (u4-pr) on main: veltc units
# tests at each head, U3c's and U4's breaks, the errors goldens compared at U4, then precheck and
# scripts/check.sh at U4 (which holds U3c).
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/9a2100e7-8c37-4a77-b4d9-1d2c2a1dfa37/scratchpad
W=/Users/andii/dev/velt/branches/d1-open-pr
L=$S/u34
mkdir -p "$L" "$S/u4"
cd "$W" || exit 2
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
unset VELT_BUILD_STAMP VELT_STD VELT_UNITS VELT_UNITS_SPLIT VELT_UNITS_BUILD VELT_UNITS_DISK
git diff --quiet || { echo "worktree not clean"; exit 2; }
back=$(git symbolic-ref --short HEAD) || { echo "not on a branch"; exit 2; }
at() { git checkout -q --detach "$(git rev-parse "$1")" && echo "== $1 $(git rev-parse --short HEAD)"; }
at u3c-pr || exit 2
cargo test -p veltc --test units > "$L/units-u3c.log" 2>&1
echo "units tests exit $?"
grep -E "^test result|FAILED" "$L/units-u3c.log"
python3 "$S/mutants.py" --list mutants_u3c
echo "U3c mutants exit $?"
git diff --quiet || git checkout -- crates
at u4-pr || exit 2
cargo test -p veltc --test units > "$L/units-u4.log" 2>&1
echo "units tests exit $?"
grep -E "^test result|FAILED" "$L/units-u4.log"
cargo test -p velt_sema --lib -- units:: > "$L/codec-u4.log" 2>&1
echo "codec tests exit $?"
grep -E "^test result|FAILED" "$L/codec-u4.log"
python3 "$S/mutants.py" --list mutants_u4
echo "U4 mutants exit $?"
git diff --quiet || git checkout -- crates
cargo build -p veltc --bin velt > "$L/build.log" 2>&1 || echo "build failed"
python3 "$S/u4_goldens.py" > "$L/goldens.txt" 2>&1
echo "goldens compare exit $?"
grep -v "^   " "$L/goldens.txt"
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
