#!/usr/bin/env bash
# #3097 gather on kernel wakes: the actors app with cross-wake's current std (head4) vs the merge
# base (base, built by run-s12.sh), tgt-s12's compiler. 3 interleaved rounds of
# `bench --only fan,cold` and `calls --scenario fan` at c=64 and c=512.
set -uo pipefail
WT=/Users/andii/dev/velt/branches/cross-wake
OUT=$HOME/s12
VELT=$HOME/tgt-s12/release/velt; RT=$HOME/tgt-s12/release/libvelt_rt.a
TIMEFORMAT='%R %U %S'
(cd $WT/examples/apps/actors && VELT_STD=$WT/std VELT_RT_LIB=$RT $VELT build --release -o $OUT/actors-head4 >$OUT/head4.build 2>&1) || { echo "build head4 failed"; tail -5 $OUT/head4.build; exit 1; }
for r in 1 2 3; do
  for s in base head4; do
    echo "== BENCH $r $s"
    $OUT/actors-$s bench --duration 1000 --shards 4 --only fan,cold | grep -E 'ops_per_sec|cycles_per_sec'
  done
  for cc in 64 512; do
    for s in base head4; do
      t=$( { time $OUT/actors-$s calls --scenario fan --c $cc --n 400000 --shards 4 >$OUT/q.out 2>/dev/null; } 2>&1 | tail -1)
      echo "CALLS $r fan c=$cc $s $(grep -o '[0-9]* ms' $OUT/q.out | tail -1) | $t"
    done
  done
done
