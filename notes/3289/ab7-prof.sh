#!/usr/bin/env bash
# After ab4.sh (same W, its compilers tc = #3256's, tc-m = main's, and trees b/m/n): per-function
# and per-line cachegrind diff of tree X against n for the rows given, built with -g at 1 thread.
# Usage: ab6-prof.sh X row... (X = b or m)
set -uo pipefail
W=$HOME/ab-3289
X=$1; shift; T=${T:-p}
mkdir -p "$W/prof7"; cd "$W/prof7"
tcof() { [ "$1" = m ] && echo "$W/tc-m" || echo "$W/tc"; }
for r in "$@"; do
  for v in "$X" "$T"; do
    tc=$(tcof $v); exe=$W/prof7/$r-$v
    VELT_STD=$W/$v-src/std VELT_RT_LIB=$tc/libvelt_rt.a "$tc/velt" build --release -g --backend llvm "$W/$v-src/bench/async/$r.velt" -o "$exe" > "$exe.build" 2>&1 || { echo "build FAIL $r-$v"; continue; }
    VELT_THREADS=1 valgrind --vgdb=no --tool=cachegrind --cache-sim=no --cachegrind-out-file="$exe.cg" "$exe" < /dev/null > /dev/null 2>&1
    cg_annotate --auto=no "$exe.cg" 2>/dev/null | sed -n '/Function:file summary/,$p' | grep -E '^[<>] ' |
      awk '{gsub(",","",$2); n=$2; f=$NF; sub(":$","",f); sub(/:\/.*/,"",f); a[f]+=n} END {for (f in a) print f, a[f]}' | sort > "fn-$r-$v.txt"
  done
  echo "### $r ($X vs n)"
  join -a1 -a2 -e0 -o 0,1.2,2.2 "fn-$r-$X.txt" "fn-$r-$T.txt" |
    awk '{d=$3-$2; a=d<0?-d:d; if (a>=100000) printf "%14d %14d %+12d  %s\n", $2, $3, d, $1}' | sort -k3 -n
  cg_annotate --diff --mod-filename="s/[bmnp]-src/X-src/" --auto=yes --context=0 --show-percs=no "$r-$X.cg" "$r-$T.cg" > "$r-$X-ldiff.txt" 2>&1
  cp "$r-$X-ldiff.txt" /Users/andii/dev/velt/branches/yield-order-fix/notes/3289/$r-$X-$T-ldiff.txt
done
echo "== prof7 end"
