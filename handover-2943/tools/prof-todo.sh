#!/bin/bash
# prof-todo.sh <label> <lazy|units>: samply profile (20 kHz) of a warm `velt build` of todo_api
# after a comment edit, to prof/todo_<label>.json; then lowering's share split by what it does.
R=/Users/andii/dev/velt/branches/d1-open-pr
T=/Users/andii/dev/velt/branches/handovers/mac-e688-2943
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ab/examples
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a VELT_THREADS=1
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
case $2 in
  units) export VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto ;;
esac
cd "$S" || exit 1
cp todo_api.velt todo_api.velt.keep
echo "// p$RANDOM" >> todo_api.velt
"$R/target/debug/velt" build todo_api.velt -o "$S/pt0" > /dev/null 2>&1
echo "// p$RANDOM" >> todo_api.velt
samply record --rate 20000 --save-only --unstable-presymbolicate -o "$T/prof/todo_$1.json" \
  "$R/target/debug/velt" build todo_api.velt -o "$S/pt1" > /dev/null 2>&1
echo "samply exit $?"
cp todo_api.velt.keep todo_api.velt
python3 -I "$T/tools/phases.py" "$T/prof/todo_$1.json" \
  'lower_decode=velt_vir::lower.*|(?=.*codec::.*dec)' \
  'decode=units::codec::.*dec|DefTable.*decode' \
  'lower=velt_vir::lower' 'sema=velt_sema::' 'codegen=velt_codegen' 'opt=velt_opt::' \
  'link=velt_link::' 'loader=velt_loader::'
