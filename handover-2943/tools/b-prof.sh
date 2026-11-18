#!/bin/bash
# b-prof.sh: samply profile of hello's units check from the fake installed prefix of b-cost.sh
# with its std read-only; then how many samples are under StdTree::fresh and under the loader.
R=/Users/andii/dev/velt/branches/d1-open-pr
T=/Users/andii/dev/velt/branches/handovers/mac-e688-2943
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad
P=$S/fake-root/toolchains/0.0.0
unset VELT_STD
export VELT_THREADS=1 VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_DISK=$S/b-units
find "$P/std" -type f -exec chmod 444 {} + ; find "$P/std" -type d -exec chmod 555 {} +
cd "$S/ab/examples" || exit 1
echo "const zq$RANDOM = 1;" >> hello.velt
"$P/bin/velt" check hello.velt > /dev/null 2>&1
echo "const zq$RANDOM = 1;" >> hello.velt
samply record --rate 20000 --save-only --unstable-presymbolicate -o "$T/prof/b_ro.json" \
  "$P/bin/velt" check hello.velt > /dev/null 2>&1
chmod -R u+w "$P/std"
python3 -I "$T/tools/phases.py" "$T/prof/b_ro.json" 'fresh=StdTree.*fresh' 'loader=velt_loader::|load_stage' 'disk_unit=units::disk::load|codec::.*dec'
