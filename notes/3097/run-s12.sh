#!/usr/bin/env bash
# #3097 steps 1+2: std-only changes, so one compiler (cross-wake's, release) and three std trees:
# base = merge base 0e3cc202e, s2 = timer-unanchor (step 2), head = cross-wake (steps 1+2). Rows:
#  1. every bench/async program, cachegrind over all threads, T=1 and T=4, 2 rounds (base, head);
#  2. xwake.velt: the c=1 cross-worker round trip (pinned caller and callee), 3 rounds each;
#  3. idle.velt: CPU of an idle program with a 10 ms ticker (the spin's cost), 2 rounds;
#  4. the actors app `calls` warm/fan at c=64 and c=512, 4 shards, 5 interleaved rounds;
#  5. `bench --duration 1000 --shards 4`, twice each, interleaved.
set -uo pipefail
WT=/Users/andii/dev/velt/branches/cross-wake
S2=/Users/andii/dev/velt/branches/timer-unanchor
BASE=${BASE:-0e3cc202e8b1c3fa0a90cf437d5a7a73eeca1a39}
B=/Users/andii/dev/velt/branches/bench-2944
OUT=$HOME/s12; mkdir -p $OUT
TGT=$HOME/tgt-s12
(cd $WT && CARGO_TARGET_DIR=$TGT CARGO_INCREMENTAL=0 cargo build --release -p veltc --bin velt 2>&1 | tail -1)
(cd $WT && CARGO_TARGET_DIR=$TGT CARGO_INCREMENTAL=0 cargo build --release -p velt_rt 2>&1 | tail -1)
VELT=$TGT/release/velt; RT=$TGT/release/libvelt_rt.a
TIMEFORMAT='%R %U %S'
rm -rf $OUT/std-base && mkdir -p $OUT/std-base && (cd $WT && git archive $BASE std | tar -x -C $OUT/std-base)
declare -A STD=([base]=$OUT/std-base/std [s2]=$S2/std [head]=$WT/std)
SIDES="base s2 head"
build() { VELT_STD=${STD[$1]} VELT_RT_LIB=$RT $VELT build --release "$2" -o "$3" >"$3.build" 2>&1 || echo "build $3 failed"; }
SRC=$WT/bench/async
progs=$(cd $SRC && ls *.velt scheduler/*.velt 2>/dev/null)
for s in base head; do
  for p in $progs; do
    n=$(echo ${p%.velt} | tr / _)
    build $s $SRC/$p $OUT/$n-$s
  done
done
for s in $SIDES; do
  build $s $B/xwake.velt $OUT/xwake-$s
  build $s $B/idle.velt $OUT/idle-$s
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
for round in 1 2 3; do
  for s in $SIDES; do
    VELT_THREADS=4 $OUT/xwake-$s | sed "s/^/XWAKE $round $s /"
  done
done
for round in 1 2; do
  for s in $SIDES; do
    r=$( { time env VELT_THREADS=4 $OUT/idle-$s >/dev/null 2>&1; } 2>&1 | tail -1)
    echo "IDLE $round $s | $r"
  done
done
for round in 1 2 3 4 5; do
  for sc in warm fan; do
    for cc in 64 512; do
      for s in $SIDES; do
        r=$( { time $OUT/actors-$s calls --scenario $sc --c $cc --n 400000 --shards 4 >$OUT/calls.out 2>/dev/null; } 2>&1 | tail -1)
        echo "CALLS $round $sc c=$cc $s $(tail -1 $OUT/calls.out | grep -o '[0-9]* ms') | $r"
      done
    done
  done
done
for round in 1 2; do
  for s in $SIDES; do
    echo "== BENCH $round $s"; $OUT/actors-$s bench --duration 1000 --shards 4 2>&1 | grep -E '▸|ops_per_sec|p50' | head -40
  done
done
