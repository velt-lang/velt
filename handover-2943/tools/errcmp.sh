#!/bin/bash
# errcmp.sh <dir> <glob>: for each matching .velt, the first error line under lazy std and under
# units from disk with VELT_UNITS_WALK_STD=1; prints the files whose error line differs.
D=$1; G=$2
R=/Users/andii/dev/velt/branches/d1-open-pr
U=/private/tmp/claude-501/-Users-andii-dev-velt-main/e68817df-6043-420f-9ebf-66ff59acdfc4/scratchpad/u8
export VELT_STD=$R/std
cd "$D" || exit 1
n=0; diff=0
for f in $G; do
  a=$("$R/target/debug/velt" check "$f" 2>&1 | grep -v '^velt' | grep -m1 'error' | sed 's/^[^ ]* //')
  b=$(VELT_UNITS_WALK_STD=1 VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_DISK=$U "$R/target/debug/velt" check "$f" 2>&1 | grep -v '^velt' | grep -m1 'error' | sed 's/^[^ ]* //')
  n=$((n+1))
  if [ "$a" != "$b" ]; then
    diff=$((diff+1))
    echo "DIFF $f"
    echo "  lazy:  ${a:0:150}"
    echo "  units: ${b:0:150}"
  fi
done
echo "$n files, $diff differ"
