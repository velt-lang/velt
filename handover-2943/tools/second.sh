#!/bin/bash
# second.sh <prog> <walk:0|1> [n]: fresh unit dir, then n builds of <prog>; errors per build.
T=/private/tmp/claude-501/-Users-andii-dev-velt-main/e68817df-6043-420f-9ebf-66ff59acdfc4/scratchpad/m3
R=/Users/andii/dev/velt/branches/d1-open-pr
U=/private/tmp/claude-501/-Users-andii-dev-velt-main/e68817df-6043-420f-9ebf-66ff59acdfc4/scratchpad/u2
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
cd "$T" || exit 1
rm -rf "${U:?}"
mkdir -p "$U"
E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=$U)
[ "$2" = 1 ] && E+=(VELT_UNITS_LIVE_WALK=1)
for i in $(seq 1 "${3:-3}"); do
  echo "const z$RANDOM = 1;" >> "$1.velt"
  out=$(env "${E[@]}" "$R/target/debug/velt" build "$1.velt" -o "$T/sec-$i.out" 2>&1)
  echo "build $i errors=$(echo "$out" | grep -c error) files=$(ls "$U" | tr '\n' ' ')"
done
