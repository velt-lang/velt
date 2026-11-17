#!/usr/bin/env bash
# #2944 rows: base (#3008's commit) vs head (the std join); each its own compiler, std and runtime.
# cachegrind over all threads; every bench/async program; T=1, T=4, T=4 VELT_RESCUE=0.
set -uo pipefail
OUT=$HOME/b2944; mkdir -p $OUT
declare -A WT=([base]=/Users/andii/dev/velt/branches/std-promise-identity [head]=/Users/andii/dev/velt/branches/std-promise-all)
SRC=/Users/andii/dev/velt/branches/std-promise-all/bench/async
progs=$(cd $SRC && ls *.velt scheduler/*.velt)
for s in base head; do
  T=$HOME/tgt-2944-$s/release
  for p in $progs; do
    n=$(echo ${p%.velt} | tr / _)
    [ -x $OUT/$n-$s ] && continue
    VELT_STD=${WT[$s]}/std VELT_RT_LIB=$T/libvelt_rt.a $T/velt build --release $SRC/$p -o $OUT/$n-$s >$OUT/$n-$s.build 2>&1 || echo "build $n-$s failed"
  done
done
irefs() { local e=$1; shift; env -u VELT_RESCUE $e valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null | awk '/I *refs:/ {gsub(",","",$NF); print $NF}'; }
configs=("T1:VELT_THREADS=1" "T4:VELT_THREADS=4" "T4R0:VELT_THREADS=4 VELT_RESCUE=0")
for round in 1 2; do
  for c in "${configs[@]}"; do
    cn=${c%%:*}; envs=${c#*:}
    for p in $progs; do
      n=$(echo ${p%.velt} | tr / _)
      for s in base head; do
        [ -x $OUT/$n-$s ] || continue
        echo "$round $cn $n $s $(irefs "$envs" $OUT/$n-$s)"
      done
    done
  done
done
