#!/bin/bash
# #3487: release build time of race_select, base std vs new, 3 interleaved uncached builds each.
set -u
W=$HOME/dev/velt/branches/fix-3420-254e
P=$HOME/dev/velt/branches/r3420-254e/perf
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
V=$W/target/release/velt
for i in 1 2 3; do
  for s in base new; do
    o=$P/bin/bt-$s-$i
    /usr/bin/time -p env VELT_BUILD_CACHE=off VELT_STD=$P/$s/std VELT_RT_LIB=$W/target/release/libvelt_rt.a "$V" build --release "$W/bench/async/race_select.velt" -o "$o" 2>&1 > /dev/null | awk -v s=$s '/^real/ {print s, $2}'
  done
done
