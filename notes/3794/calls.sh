#!/bin/bash
# The calls (bl/b to symbols) in one function, base vs head.
OUT=$HOME/s13; row=$1; sym=$2
for side in base head; do
  objdump -d --no-show-raw-insn "$OUT/bin/$row-$side" | awk -v s="$sym" '/^[0-9a-f]+ <.*>:$/ {p = index($0, s) > 0} p' | grep -oE "\s(bl|b)\s+[0-9a-f]+ <[^+>]*>" | sed -E 's/.*<//; s/>//' | sort | uniq -c > "$OUT/cg/calls-$side.txt"
done
diff "$OUT/cg/calls-base.txt" "$OUT/cg/calls-head.txt"
