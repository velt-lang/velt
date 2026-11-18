#!/bin/bash
# Which part of steps 1+gather costs the actors cold cycle: the cold cycle built with one compiler
# and base std, head std, head without gather-back (sched_gather.velt `gathers` false) and head
# without the spin (sched_spin.velt `spin` false), interleaved ROUNDS times at VELT_THREADS=T.
#   orb -m velt-prof bash notes/3097/vm-fix/cold-ab.sh [rounds] [threads]
set -u
OUT=$HOME/b3097fix
WT=/Users/andii/dev/velt/branches/cross-wake
V=${VELT:-$OUT/tgt/release/velt}
RT=$OUT/tgt/release/libvelt_rt.a
rounds=${1:-7}; th=${2:-4}
for v in nogather nospin; do
  rm -rf "$OUT/std-$v" && cp -r "$WT/std" "$OUT/std-$v"
done
sed -i '/^export function gathers(e: usize, g: usize): bool {$/a\  return false;' "$OUT/std-nogather/lang/hooks/sched_gather.velt"
sed -i '/^export function spin(e: usize): bool {$/a\  return false;' "$OUT/std-nospin/lang/hooks/sched_spin.velt"
declare -A STD=([base]=$OUT/base-std [head]=$WT/std [nogather]=$OUT/std-nogather [nospin]=$OUT/std-nospin)
sides="base head nogather nospin"
cd "$WT/examples/apps/actors" || exit 1
for s in $sides; do
  VELT_STD=${STD[$s]} VELT_RT_LIB=$RT "$V" build --release -o "$OUT/cold-$s" >"$OUT/cold-$s.build" 2>&1 \
    || { echo "build $s failed"; tail -3 "$OUT/cold-$s.build"; exit 1; }
done
for r in $(seq 1 "$rounds"); do
  line="round $r:"
  for s in $sides; do
    v=$(VELT_THREADS=$th "$OUT/cold-$s" bench --duration 2000 --shards 4 --only cold 2>&1 \
      | awk '/cycles_per_sec/ {print $2}')
    line="$line  $s $v"
  done
  echo "$line"
done
