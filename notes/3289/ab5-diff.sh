#!/usr/bin/env bash
# Line-level diff of the stack profiles (ab5-prof.sh): cg_diff of m and n per row, source paths
# made comparable, then the source lines whose counts changed most.
set -uo pipefail
W=$HOME/ab-stack/prof
cd "$W"
for r in "$@"; do
  cg_diff --mod-filename='s#/home/[^/]*/ab-stack/[mnb]-src/##' "$r-m.cg" "$r-n.cg" > "$r-diff.cg" 2>/dev/null
  echo "### $r: lines that changed most (n - m)"
  cg_annotate --auto=yes --context=0 --show-percs=no "$r-diff.cg" 2>/dev/null |
    awk '/^-- Auto-annotated source: /{f=$4} /^ *-?[0-9,]+ +[^ ]/ { v=$1; gsub(",","",v); if (v+0 >= 400000 || v+0 <= -400000) { line=$0; sub(/^ *-?[0-9,]+ +/, "", line); printf "%12s  %-45s %s\n", $1, f, substr(line,1,90) } }' |
    sort -t' ' -k1 -rn | head -40
done
echo "== diff end"
