#!/bin/bash
# vm-dhat.sh <label>: (in the VM) DHAT heap profile of a warm units `velt build` of todo_api
# after a comment edit; dhat.out kept in branches/handovers/mac-e688-2943/prof/vm-<label>/.
source "$HOME/.cargo/env"
L=$1
R=/Users/andii/dev/velt/branches/d1-open-pr
OUT=/Users/andii/dev/velt/branches/handovers/mac-e688-2943/prof/vm-$L
V=$HOME/tgt-dhat/debug/velt
W=$HOME/vmab
mkdir -p "$OUT"
rm -rf "$W" && mkdir -p "$W"
cp -R "$R/examples" "$W/examples" && rm -rf "$W/examples/apps" "$W/examples/target"
mkdir -p "$W/packages" && cp -R "$R/packages/http-server" "$W/packages/"
export VELT_STD=$R/std VELT_RT_LIB=$HOME/tgt-dhat/debug/libvelt_rt.a VELT_THREADS=1 VELT_SEMA_THREADS=1
export VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto
cd "$W/examples" || exit 1
for i in 1 2; do echo "// warm $RANDOM" >> todo_api.velt; "$V" build todo_api.velt -o "$W/w" > /dev/null 2>&1; done
echo "// edit $RANDOM" >> todo_api.velt
valgrind --tool=dhat --dhat-out-file="$OUT/dhat-todo_api-units.json" "$V" build todo_api.velt -o "$W/o" > "$OUT/run-dhat.log" 2>&1
echo "dhat exit $?"
grep -E "Total:|At t-gmax|Reads|Writes" "$OUT/run-dhat.log" | head -6
rm -rf "$W"
