#!/usr/bin/env bash
# Per-line Ir of some std files for one profiled tree: ann.sh <tree> <row> <file-substring>...
W=$HOME/ab-3289/prof7; v=$1; r=$2; shift 2
for f in "$@"; do
  echo "#### $v $f"
  cg_annotate --auto=yes --context=0 --show-percs=no --threshold=0 "$W/$r-$v.cg" 2>/dev/null |
    awk -v f="$f" '/^-- Annotated source file:/ {on = index($0, f) > 0} on && /^ *[0-9,]+  / && !/^ *0  /' | grep -v "^-- line" | head -70
done
