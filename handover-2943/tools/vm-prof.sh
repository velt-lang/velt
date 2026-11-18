#!/bin/bash
# vm-prof.sh <label>: (in the OrbStack VM velt-prof) instruction profiles of warm `velt build`s
# after a comment edit: todo_api and hello, lazy std and units, with cachegrind (instructions
# only), and callgrind for units todo_api (inclusive costs). Profiles and annotations are kept in
# branches/handovers/mac-e688-2943/prof/vm-<label>/ (outside the VM build dir, which is deleted
# after). Uses $HOME/tgt-7de7 (vm-build.sh).
source "$HOME/.cargo/env"
L=$1
R=/Users/andii/dev/velt/branches/d1-open-pr
OUT=/Users/andii/dev/velt/branches/handovers/mac-e688-2943/prof/vm-$L
V=$HOME/tgt-7de7/debug/velt
W=$HOME/vmab
mkdir -p "$OUT"
rm -rf "$W" && mkdir -p "$W"
cp -R "$R/examples" "$W/examples" && rm -rf "$W/examples/apps" "$W/examples/target"
mkdir -p "$W/packages" && cp -R "$R/packages/http-server" "$W/packages/"
cp "$R/tests/golden/m1/hello.velt" "$W/examples/hello.velt"
export VELT_STD=$R/std VELT_RT_LIB=$HOME/tgt-7de7/debug/libvelt_rt.a VELT_THREADS=1 VELT_SEMA_THREADS=1
cd "$W/examples" || exit 1
{
  echo "commit $(git -C "$R" rev-parse --short HEAD) + worktree changes: $(git -C "$R" status --short | grep -v '^??' | tr '\n' ' ')"
  echo "velt $(ls -la "$V" | awk '{print $6, $7, $8}')"
} > "$OUT/README.txt"
for p in todo_api hello; do
  for mode in lazy units; do
    case $mode in
      lazy) E=() ;;
      units) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto) ;;
    esac
    for i in 1 2; do
      echo "// warm $RANDOM" >> "$p.velt"
      env "${E[@]}" "$V" build "$p.velt" -o "$W/w-$p" > /dev/null 2>&1
    done
    echo "// edit $RANDOM" >> "$p.velt"
    f="$OUT/cg-$p-$mode.out"
    env "${E[@]}" valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file="$f" \
      "$V" build "$p.velt" -o "$W/o-$p-$mode" > "$OUT/run-$p-$mode.log" 2>&1
    echo "$p $mode exit $?: $(grep -m1 'I *refs' "$OUT/run-$p-$mode.log")" | tee -a "$OUT/README.txt"
    cg_annotate --show-percs=yes "$f" > "$OUT/cg-$p-$mode.txt" 2>&1
  done
done
# Inclusive costs for the units build of todo_api.
E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto)
echo "// edit $RANDOM" >> todo_api.velt
env "${E[@]}" valgrind --tool=callgrind --callgrind-out-file="$OUT/callgrind-todo_api-units.out" \
  "$V" build todo_api.velt -o "$W/o-cgr" > "$OUT/run-callgrind.log" 2>&1
echo "callgrind exit $?" | tee -a "$OUT/README.txt"
callgrind_annotate --inclusive=yes "$OUT/callgrind-todo_api-units.out" > "$OUT/callgrind-todo_api-units-inclusive.txt" 2>&1
callgrind_annotate "$OUT/callgrind-todo_api-units.out" > "$OUT/callgrind-todo_api-units-self.txt" 2>&1
rm -rf "$W"
ls -la "$OUT" | awk '{print $5, $9}'
