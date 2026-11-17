#!/usr/bin/env bash
# Gather-back's idle threshold: the actors app with std copies at IDLE_PCT 90, 75 and 50 (cross-wake
# plus the spin's idle time) against base, tgt-s12's compiler. Rows, interleaved, 3 rounds:
# `bench --only fan,cold` (fan c=1/64/512 ops, then cold-cycle), and `calls --scenario fan` at
# c=64 and c=512 (ms, cores busy).
set -uo pipefail
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/505bcb47-021f-409e-bf01-c29e5c3a75e5/scratchpad
WT=/Users/andii/dev/velt/branches/cross-wake
OUT=$HOME/s12
VELT=$HOME/tgt-s12/release/velt; RT=$HOME/tgt-s12/release/libvelt_rt.a
TIMEFORMAT='%R %U %S'
for v in 90 75 50; do
  (cd $WT/examples/apps/actors && VELT_STD=$S/idle$v/std VELT_RT_LIB=$RT $VELT build --release -o $OUT/actors-i$v >/dev/null 2>&1) || echo "build i$v failed"
done
SIDES="base i90 i75 i50"
for r in 1 2 3; do
  for s in $SIDES; do
    echo "== BENCH $r $s"
    $OUT/actors-$s bench --duration 1000 --shards 4 --only fan,cold | grep -E 'ops_per_sec|cycles_per_sec'
  done
  for cc in 64 512; do
    for s in $SIDES; do
      t=$( { time $OUT/actors-$s calls --scenario fan --c $cc --n 400000 --shards 4 >$OUT/sw.out 2>/dev/null; } 2>&1 | tail -1)
      echo "CALLS $r fan c=$cc $s $(grep -o '[0-9]* ms' $OUT/sw.out | tail -1) | $t"
    done
  done
done
