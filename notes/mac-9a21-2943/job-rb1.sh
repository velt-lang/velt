#!/bin/bash
# job-rb1.sh, in branches/d1-open-pr's target: the U1 stack rebased on main (rb-codec .. rb-ctx):
# codec tests at U1a (with velt_syntax's), U1c and the top, then precheck and scripts/check.sh
# at the top (rb-ctx, which holds U1a-U1e).
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/9a2100e7-8c37-4a77-b4d9-1d2c2a1dfa37/scratchpad
W=/Users/andii/dev/velt/branches/d1-open-pr
L=$S/rb1
mkdir -p "$L"
cd "$W" || exit 2
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
unset VELT_BUILD_STAMP VELT_STD VELT_UNITS VELT_UNITS_SPLIT VELT_UNITS_BUILD VELT_UNITS_DISK
git diff --quiet || { echo "worktree not clean"; exit 2; }
back=$(git symbolic-ref --short HEAD) || { echo "not on a branch"; exit 2; }
at() { git checkout -q --detach "$(git rev-parse "$1")" && echo "== $1 $(git rev-parse --short HEAD)"; }
at rb-codec || exit 2
cargo test -p velt_syntax -p velt_sema --lib codec > "$L/tests-rb-codec.log" 2>&1
echo "tests exit $?"
grep -E "^test result|FAILED|panicked" "$L/tests-rb-codec.log" | cut -c1-200
for b in rb-defs rb-ctx; do
  at "$b" || exit 2
  cargo test -p velt_sema --lib -- ctx:: units:: ends_on_damaged_input > "$L/tests-$b.log" 2>&1
  echo "tests exit $?"
  grep -E "^test result|FAILED|panicked|overflow|at the unit's end" "$L/tests-$b.log" | cut -c1-300 | head -20
done
python3 "$S/mutants.py" --list mutants_ctx "decode stack" > "$L/mutant-stack.txt" 2>&1
echo "decode-stack mutant exit $? (0: caught)"
cat "$L/mutant-stack.txt"
git diff --quiet || git checkout -- crates
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
