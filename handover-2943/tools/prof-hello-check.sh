#!/bin/bash
# prof-hello-check.sh <label>: samply profile of a warm units `velt check` of hello (after a
# one-line edit, unit on disk), saved to prof/hellochk_<label>.json; then the phases.
R=/Users/andii/dev/velt/branches/d1-open-pr
T=/Users/andii/dev/velt/branches/handovers/mac-e688-2943
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ab/examples
export VELT_STD=$R/std VELT_THREADS=1
export VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_DISK=auto
cd "$S" || exit 1
echo "const zk$RANDOM = 1;" >> hello.velt
"$R/target/debug/velt" check hello.velt > /dev/null 2>&1
echo "const zk$RANDOM = 1;" >> hello.velt
samply record --save-only --unstable-presymbolicate -o "$T/prof/hellochk_$1.json" \
  "$R/target/debug/velt" check hello.velt > /dev/null 2>&1
echo "samply exit $?"
python3 -I "$T/tools/phases.py" "$T/prof/hellochk_$1.json" \
  'loader=velt_loader::|load_stage' \
  'disk_unit=units::disk::load|units::app_unit|codec::.*dec' \
  'copy_std=units: copy std|Ctx as core::clone::Clone|clone::Clone>::clone' \
  'local_async=local_async' 'live_walk=units_live_set|walk_units_program' \
  'ownership=ownership' 'generated=generated::' 'rounds=rounds::run' \
  'sema_other=velt_sema::' 'exit_free=drop_glue|mi_free|exit'
