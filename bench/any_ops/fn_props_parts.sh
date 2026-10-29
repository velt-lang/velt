#!/bin/bash
# Instructions of parts of bench/any_ops/fn_props.vlt, each built alone after `sync(1)` (so every
# variant gives functions properties), with the base and branch toolchains of a compiler_ab work
# dir: parts.sh <work dir> [-g]
W=$1
G=${2:-}
D=$W/parts; mkdir -p $D
src=$W/branch-src/bench/any_ops/fn_props.vlt
grep -v -e '^console.log(' -e '^// In a task' $src > $D/body.vlt
i=0
for part in "0" "sync(1500000)" "await later(1500000)" "closures(1000000)" \
  "await spawn(locked(1000000))" "await spawn(lockedProps(1000000))" "await spawn(reads(1000000))" \
  "await spawn(promises(300000))"; do
  i=$((i+1)); n=p$i
  { cat $D/body.vlt; echo "sync(1);"; echo "console.log($part);"; } > $D/$n.vlt
  for v in base branch; do
    VELT_STD=$W/$v-src/std VELT_RT_LIB=$W/tc-$v/libvelt_rt.a $W/tc-$v/velt build --release $G --backend llvm $D/$n.vlt -o $D/$n-$v > $D/$n-$v.build 2>&1 || { echo "$part $v FAIL"; continue; }
    ir=$(cd $D && VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=$D/cg.$n-$v ./$n-$v 2>&1 >/dev/null | grep -oP "I\s+refs:\s+\K[0-9,]+" | tr -d ,)
    echo "$part | $v | $ir | $($D/$n-$v 2>/dev/null | tail -1)"
  done
done
