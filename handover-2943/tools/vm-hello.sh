#!/bin/bash
# vm-hello.sh <label>: (in the OrbStack VM velt-prof) callgrind of a warm `velt build` of hello,
# lazy std and from std's unit on disk, after a comment edit, each mode to its own output path
# reused across its builds (vpm probe: the stamp keeps the read files' hashes). Inclusive
# annotations per mode in prof/vm-<label>/. Uses $HOME/tgt-7de7 (vm-build.sh).
source "$HOME/.cargo/env"
L=$1
R=/Users/andii/dev/velt/branches/d1-open-pr
OUT=/Users/andii/dev/velt/branches/handovers/mac-e688-2943/prof/vm-$L
V=$HOME/tgt-7de7/debug/velt
W=$HOME/vmhello
mkdir -p "$OUT"
rm -rf "$W" && mkdir -p "$W"
cp "$R/tests/golden/m1/hello.velt" "$W/hello.velt"
export VELT_STD=$R/std VELT_RT_LIB=$HOME/tgt-7de7/debug/libvelt_rt.a VELT_THREADS=1 VELT_SEMA_THREADS=1
cd "$W" || exit 1
echo "commit $(git -C "$R" rev-parse --short HEAD)" > "$OUT/README.txt"
for mode in lazy units; do
  case $mode in
    lazy) E=() ;;
    units) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=$W/units) ;;
  esac
  for i in 1 2 3; do
    echo "// warm $RANDOM" >> hello.velt
    env "${E[@]}" "$V" build hello.velt -o "$W/o-$mode" > /dev/null 2>&1
  done
  echo "// edit $RANDOM" >> hello.velt
  f="$OUT/callgrind-hello-$mode.out"
  env "${E[@]}" valgrind --tool=callgrind --callgrind-out-file="$f" "$V" build hello.velt \
    -o "$W/o-$mode" > "$OUT/run-$mode.log" 2>&1
  echo "$mode exit $?" | tee -a "$OUT/README.txt"
  callgrind_annotate --inclusive=yes "$f" > "$OUT/inclusive-$mode.txt" 2>&1
  grep -m1 "PROGRAM TOTALS" "$OUT/inclusive-$mode.txt" | tee -a "$OUT/README.txt"
done
rm -rf "$W"
