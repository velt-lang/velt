#!/bin/bash
# The actors cold cycle (bench --only cold, c=1 activate/deactivate) built with one compiler and
# base std, step 2's std (89b137039) and head std (HEAD_STD, default the worktree's), interleaved
# ROUNDS times at VELT_THREADS=T; cycles/s per run.
#   orb -m velt-prof bash notes/3097/vm-fix/cold.sh [rounds] [threads]
set -u
OUT=$HOME/b3097fix
WT=/Users/andii/dev/velt/branches/cross-wake
V=$OUT/tgt/release/velt
RT=$OUT/tgt/release/libvelt_rt.a
rounds=${1:-5}; th=${2:-4}
[ -d "$OUT/std-89b137039" ] || bash "$(dirname "$0")/export.sh" 89b137039 >/dev/null
declare -A STD=([base]=$OUT/base-std [s2]=$OUT/std-89b137039 [head]=${HEAD_STD:-$WT/std})
cd "$WT/examples/apps/actors" || exit 1
for s in base s2 head; do
  VELT_STD=${STD[$s]} VELT_RT_LIB=$RT "$V" build --release -o "$OUT/actors-$s" >"$OUT/actors-$s.build" 2>&1 \
    || { echo "build $s failed"; tail -3 "$OUT/actors-$s.build"; exit 1; }
done
for r in $(seq 1 "$rounds"); do
  line="round $r:"
  for s in base s2 head; do
    v=$(VELT_THREADS=$th "$OUT/actors-$s" bench --duration 2000 --shards 4 --only cold 2>&1 \
      | awk '/cycles_per_sec/ {print $2, $4}')
    line="$line  $s $v"
  done
  echo "$line"
done
