#!/bin/bash
# prof-hello.sh <label>: samply profile of a warm units `velt build` of hello (after a one-line
# edit, unit on disk), saved to prof/hello_<label>.json; then the top functions (prof.py).
R=/Users/andii/dev/velt/branches/d1-open-pr
T=/Users/andii/dev/velt/branches/handovers/mac-e688-2943
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ab/examples
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a VELT_THREADS=1
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
export VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto
cd "$S" || exit 1
# Warm: the unit on disk and the build cache filled.
echo "const zp$RANDOM = 1;" >> hello.velt
"$R/target/debug/velt" build hello.velt -o "$S/hp0" > /dev/null 2>&1
echo "const zp$RANDOM = 1;" >> hello.velt
samply record --rate 20000 --save-only --unstable-presymbolicate -o "$T/prof/hello_$1.json" \
  "$R/target/debug/velt" build hello.velt -o "$S/hp1" > /dev/null 2>&1
echo "samply exit $?"
python3 -I "$T/tools/prof.py" "$T/prof/hello_$1.json" 45
