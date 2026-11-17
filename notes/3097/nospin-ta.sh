#!/usr/bin/env bash
# timer_await (and timer_await_top) at T=4: instructions of base, head (steps 1+2+gather) and head
# without the spin (a std copy whose spin returns at once), each built by tgt-s12's compiler.
set -uo pipefail
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/505bcb47-021f-409e-bf01-c29e5c3a75e5/scratchpad
WT=/Users/andii/dev/velt/branches/cross-wake
OUT=$HOME/s12
VELT=$HOME/tgt-s12/release/velt; RT=$HOME/tgt-s12/release/libvelt_rt.a
for p in timer_await timer_await_top; do
  VELT_STD=$WT/std VELT_RT_LIB=$RT $VELT build --release $WT/bench/async/$p.velt -o $OUT/$p-head2 >/dev/null 2>&1 || echo "build $p head2 failed"
  VELT_STD=$S/nospin/std VELT_RT_LIB=$RT $VELT build --release $WT/bench/async/$p.velt -o $OUT/$p-nospin >/dev/null 2>&1 || echo "build $p nospin failed"
  for r in 1 2; do
    for s in base head2 nospin; do
      n=$(VELT_THREADS=4 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null $OUT/$p-$s 2>&1 >/dev/null | awk '/I *refs:/ {gsub(",","",$NF); print $NF}')
      echo "$p $r $s $n"
    done
  done
done
