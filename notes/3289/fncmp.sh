#!/usr/bin/env bash
# Per-function instruction totals of row $1, tree b vs c (from ab3-prof.sh's .cg files).
cd "$HOME/ab-3241b/prof" || exit 1
for v in b c; do
  cg_annotate --auto=no "$1-$v.cg" 2>/dev/null | sed -n '/Function:file summary/,$p' | grep -E '^[<>] ' |
    awk '{gsub(",","",$2); n=$2; f=$NF; sub(":$","",f); print f, n}' | sort > "fn-$1-$v.txt"
done
join -a1 -a2 -e0 -o 0,1.2,2.2 "fn-$1-b.txt" "fn-$1-c.txt" |
  awk '{d=$3-$2; if (d<0) a=-d; else a=d; if (a>=200000) printf "%14d %14d %+12d  %s\n", $2, $3, d, $1}' | sort -k3 -n
