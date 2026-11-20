#!/bin/bash
# u4_rounds.sh <golden under tests/golden>: the round and pass lines of VELT_SEMA_TIMINGS for one
# golden with VELT_UNITS=2, std checked in process vs std's unit loaded from disk (the velt built
# last in branches/d1-open-pr, with the U4 draft's std).
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/9a2100e7-8c37-4a77-b4d9-1d2c2a1dfa37/scratchpad
W=/Users/andii/dev/velt/branches/d1-open-pr
g="$W/tests/golden/$1"
disk=$(mktemp -d /tmp/claude-u4r.XXXX)
export VELT_STD=$S/u4-std/std VELT_SEMA_TIMINGS=1
cd "$(dirname "$g")" || exit 2
VELT_UNITS=2 VELT_UNITS_DISK=$disk "$W/target/debug/velt" check "$(basename "$g")" > /dev/null 2>&1
env -u VELT_UNITS_DISK VELT_UNITS=2 "$W/target/debug/velt" check "$(basename "$g")" 2> "$S/u4r-plain.txt" > /dev/null
VELT_UNITS=2 VELT_UNITS_DISK=$disk "$W/target/debug/velt" check "$(basename "$g")" 2> "$S/u4r-load.txt" > /dev/null
rm -rf "$disk"
for f in plain load; do
  echo "== $f: $(grep -c "round" "$S/u4r-$f.txt") round lines"
  grep -i "round" "$S/u4r-$f.txt" | sed -E 's/[0-9]+\.[0-9]+ ?ms//g' | sort | uniq -c | head -12
done
