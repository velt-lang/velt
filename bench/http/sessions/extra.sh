#!/bin/bash
# Repeat instruction counts (3 runs per side) and run the soundness probes on both sides.
export OHA=$HOME/velt-build/smp/oha116
cd ~/velt/branches/shared-marked-proto/bench/http/sessions
source ../lib.sh
B=~/velt/branches/smp-base P=~/velt/branches/shared-marked-proto
for s in base proto; do
  t=$B; v=~/velt-build/smp-base2/release/velt
  [ $s = proto ] && t=$P && v=~/velt-build/smp-proto/release/velt
  VELT_STD=$t/std $v build --release $t/bench/http/techempower/server.velt -o $WORK/te-$s >&2
  VELT_STD=$t/std $v build --release $t/bench/http/sessions/server.velt -o $WORK/se-$s >&2
  for p in race move_noclosure bis_b; do
    VELT_STD=$t/std $v build $P/bench/http/sessions/probes/$p.velt -o $WORK/$p-$s >&2
  done
done
for path in /plaintext /json; do
  for r in 1 2 3; do
    for s in base proto; do PORT=$(random_port); echo "ir $path $s run$r $(ir_per_request $path 2000 12000 env VELT_THREADS=1 $WORK/te-$s $PORT)"; done
  done
done
for r in 1 2; do
  for s in base proto; do PORT=$(random_port); echo "ir sessions $s run$r $(ir_per_request /x 2000 12000 env VELT_THREADS=1 $WORK/se-$s $PORT)"; done
done
ulimit -c 0
for s in base proto; do
  for p in race move_noclosure bis_b; do
    for r in 1 2 3 4 5; do
      echo "probe $p $s run$r: $(VELT_RC_STATS=1 VELT_THREADS=8 timeout 120 $WORK/$p-$s 2>&1 | tail -2 | tr "\n" " ") exit=$?"
    done
  done
done
