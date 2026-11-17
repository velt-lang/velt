#!/usr/bin/env bash
# #3097 step 2 (timer-unanchor): a std-only change, so one compiler (the branch's, release) and
# two std trees: base = the merge base's std, head = the branch's. Rows:
#  1. every bench/async program, cachegrind over all threads, T=1 and T=4, 2 rounds;
#  2. the actors app: `calls` warm/fan at c=64 and c=512, 4 shards, wall + CPU (cores busy),
#     5 interleaved rounds; `bench --duration 1000` twice each, interleaved.
set -uo pipefail
WT=/Users/andii/dev/velt/branches/timer-unanchor
BASE=${BASE:?merge base}
OUT=$HOME/s2; mkdir -p $OUT
TGT=$HOME/tgt-s2
(cd $WT && CARGO_TARGET_DIR=$TGT CARGO_INCREMENTAL=0 cargo build --release -p veltc --bin velt 2>&1 | tail -1)
(cd $WT && CARGO_TARGET_DIR=$TGT CARGO_INCREMENTAL=0 cargo build --release -p velt_rt 2>&1 | tail -1)
TIMEFORMAT='%R %U %S'
VELT=$TGT/release/velt; RT=$TGT/release/libvelt_rt.a
rm -rf $OUT/std-base && mkdir -p $OUT/std-base && (cd $WT && git archive $BASE std | tar -x -C $OUT/std-base)
declare -A STD=([base]=$OUT/std-base/std [head]=$WT/std)
SRC=$WT/bench/async
progs=$(cd $SRC && ls *.velt scheduler/*.velt 2>/dev/null)
for s in base head; do
  for p in $progs; do
    n=$(echo ${p%.velt} | tr / _)
    VELT_STD=${STD[$s]} VELT_RT_LIB=$RT $VELT build --release $SRC/$p -o $OUT/$n-$s >$OUT/$n-$s.build 2>&1 || echo "build $n-$s failed"
  done
  (cd $WT/examples/apps/actors && VELT_STD=${STD[$s]} VELT_RT_LIB=$RT $VELT build --release -o $OUT/actors-$s >$OUT/actors-$s.build 2>&1) || echo "build actors-$s failed"
done
irefs() { local e=$1; shift; env -u VELT_RESCUE $e valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null | awk '/I *refs:/ {gsub(",","",$NF); print $NF}'; }
for round in 1 2; do
  for c in "T1:VELT_THREADS=1" "T4:VELT_THREADS=4"; do
    cn=${c%%:*}; envs=${c#*:}
    for p in $progs; do
      n=$(echo ${p%.velt} | tr / _)
      for s in base head; do
        [ -x $OUT/$n-$s ] || continue
        echo "IR $round $cn $n $s $(irefs "$envs" $OUT/$n-$s)"
      done
    done
  done
done
for round in 1 2 3 4 5; do
  for sc in warm fan; do
    for cc in 64 512; do
      for s in base head; do
        r=$( { time $OUT/actors-$s calls --scenario $sc --c $cc --n 400000 --shards 4 >$OUT/calls.out 2>/dev/null; } 2>&1 | tail -1)
        echo "CALLS $round $sc c=$cc $s $(tail -1 $OUT/calls.out | grep -o '[0-9]* ms') | $r"
      done
    done
  done
done
for round in 1 2; do
  for s in base head; do
    echo "== BENCH $round $s"; $OUT/actors-$s bench --duration 1000 --shards 4 2>&1 | grep -E '▸|ops_per_sec|p50' | head -40
  done
done
