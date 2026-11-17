#!/usr/bin/env bash
# fan (1000 keys) at c=64 and c=512, 4 shards: wall ms and cores busy ((user+sys)/real), base vs
# s2 vs head, the actors builds cold-mac.sh made, 3 interleaved rounds.
set -uo pipefail
OUT=/private/tmp/claude-501/-Users-andii-dev-velt-main/505bcb47-021f-409e-bf01-c29e5c3a75e5/scratchpad/xw
for r in 1 2 3; do
  for cc in 64 512; do
    for s in base s2 head; do
      /usr/bin/time -p "$OUT/actors-$s" calls --scenario fan --c $cc --n 400000 --shards 4 >"$OUT/fan.out" 2>"$OUT/fan.time"
      real=$(awk '/^real/ {print $2}' "$OUT/fan.time")
      cpu=$(awk '/^user|^sys/ {s += $2} END {print s}' "$OUT/fan.time")
      echo "c=$cc $s: $(grep -o '[0-9]* ms' "$OUT/fan.out" | tail -1) cores $(echo "scale=2; $cpu / $real" | bc)"
    done
  done
done
