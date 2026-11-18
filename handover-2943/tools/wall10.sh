#!/bin/bash
# wall10.sh <velt>: wall time of warm hello builds (comment edit, same output path), lazy std and
# from std's unit on disk, interleaved: 10 rounds of one build each, printed per mode as the
# median and best of the 10 (ms). VELT_THREADS unset (the default a user gets).
V=$1
R=/Users/andii/dev/velt/branches/d1-open-pr
D=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/wall10
rm -rf "$D" && mkdir -p "$D/u"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk VELT_STD=$R/std
export VELT_RT_LIB=$R/target/debug/libvelt_rt.a
cp "$R/tests/golden/m1/hello.velt" "$D/h.velt"
U=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK="$D/u")
build() { # mode
  echo "// $1 $RANDOM" >> "$D/h.velt"
  local t0 t1
  t0=$(python3 -I -c 'import time; print(time.perf_counter_ns())')
  case $1 in
    lazy) "$V" build "$D/h.velt" -o "$D/o-lazy" > /dev/null 2>&1 ;;
    units) env "${U[@]}" "$V" build "$D/h.velt" -o "$D/o-units" > /dev/null 2>&1 ;;
  esac
  t1=$(python3 -I -c 'import time; print(time.perf_counter_ns())')
  echo $(( (t1 - t0) / 1000000 ))
}
build lazy > /dev/null; build units > /dev/null; build lazy > /dev/null; build units > /dev/null
: > "$D/lazy.txt"; : > "$D/units.txt"
for i in $(seq 10); do
  build lazy >> "$D/lazy.txt"
  build units >> "$D/units.txt"
done
for m in lazy units; do
  printf '%-6s median %4s ms  best %4s ms\n' "$m" "$(sort -n "$D/$m.txt" | sed -n 5p)" "$(sort -n "$D/$m.txt" | head -1)"
done
