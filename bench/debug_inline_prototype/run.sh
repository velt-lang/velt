#!/usr/bin/env bash
# usage: run.sh VELT STD LABEL [env...] : debug-build each program and count its run under cachegrind
VELT=$1; STD=$2; L=$3; shift 3
M=~/velt-build/debug-inline/mp; O=$M/o/$L; mkdir -p $O
for f in ${PROGS:-$M/p/*.velt}; do n=$(basename $f .velt); [ $n = hello ] && continue
  env "$@" VELT_STD=$STD VELT_HOME=$M/home-$L VELT_BUILD_CACHE=off $VELT build $f -o $O/$n > $O/$n.bl 2>&1 || { echo "$n BUILD-FAIL"; head -5 $O/$n.bl; continue; }
  c=$(VELT_THREADS=1 VELT_RESCUE=0 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null $O/$n 2>&1 >/dev/null </dev/null | awk '/I +refs/{gsub(",","",$NF);print $NF}')
  echo -e "$n\t$c"
done | tee $M/o/$L.tsv
