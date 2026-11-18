#!/bin/bash
# cg-misses.sh: how many functions a debug build of todo_api recompiles after each kind of
# one-line edit (the codegen line of --timings), lazy std and units.
R=/Users/andii/dev/velt/branches/d1-open-pr
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ab/examples
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a VELT_THREADS=1
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
cd "$S" || exit 1
cp todo_api.velt todo_api.velt.orig
for mode in lazy units; do
  case $mode in
    lazy) E=() ;;
    units) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto) ;;
  esac
  build() {
    env "${E[@]}" "$R/target/debug/velt" build todo_api.velt -o "$S/cg-$mode-$RANDOM" --timings 2>&1 \
      | grep -E "^velt: codegen" | sed 's/.*(\(.*\))/\1/'
  }
  cp todo_api.velt.orig todo_api.velt
  echo "$mode warm-up: $(build)"
  echo "// a comment $RANDOM" >> todo_api.velt
  echo "$mode comment: $(build)"
  echo "const zc = 1;" >> todo_api.velt
  echo "$mode new const: $(build)"
  echo "// another comment $RANDOM" >> todo_api.velt
  echo "$mode same const, comment: $(build)"
done
cp todo_api.velt.orig todo_api.velt
rm -f "$S"/cg-*
