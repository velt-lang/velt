#!/usr/bin/env bash
# Does cold-cycle converge back after the fan rows? The actors app with cross-wake's current std
# (head) and the merge base's (base), tgt-s12's compiler; `bench --only fan,cold`, 4 runs of head.
set -uo pipefail
WT=/Users/andii/dev/velt/branches/cross-wake
OUT=$HOME/s12
VELT=$HOME/tgt-s12/release/velt; RT=$HOME/tgt-s12/release/libvelt_rt.a
(cd $WT/examples/apps/actors && VELT_STD=$WT/std VELT_RT_LIB=$RT $VELT build --release -o $OUT/actors-head3 >/dev/null 2>&1) || echo "build head3 failed"
echo "== base"; $OUT/actors-base bench --duration 500 --shards 4 --only fan,cold | grep -A1 cold-cycle | tail -1
for r in 1 2 3 4; do
  echo "== head3 $r"; $OUT/actors-head3 bench --duration 500 --shards 4 --only fan,cold | grep -A1 cold-cycle | tail -1
done
