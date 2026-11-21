#!/bin/bash
# In the VM: instructions per function (callgrind's inlined-source rows summed by function) for
# a row's base and head profiles, and the difference, largest first.
#   orb -m velt-prof bash vm-agg.sh <row>
OUT=$HOME/r3273/cg
row=$1
for side in base head; do
  callgrind_annotate --threshold=100 "$OUT/$row-$side.out" 2>/dev/null | grep -E "^ *[0-9,]+ \(" |
    sed -E 's/ \[[^]]*\]$//' | awk '{n=$1; gsub(",", "", n); f=$NF; sub(/.*:/, "", f); sub(/\x27[0-9]+$/, "", f); t[f]+=n} END{for (f in t) print t[f], f}' | sort -k2 > "$OUT/$row-$side.fn"
done
join -1 2 -2 2 -a 1 -a 2 -e 0 -o 0,1.1,2.1 "$OUT/$row-base.fn" "$OUT/$row-head.fn" | awk '{d=$3-$2; if (d>20000 || d<-20000) printf "%+9d  %9d -> %9d  %s\n", d, $2, $3, $1}' | sort -rn | cut -c1-120
