#!/bin/bash
# failfiles.sh <val-dir>...: the failing golden files of each run, both report forms
# (`<file> [mode]: …` and an error test's `<file>: stderr …`), written to <dir>/fail_files.txt;
# prints the count, and what changed from the run before.
S=/Users/andii/dev/velt/branches/handovers/mac-e688-2943
prev=
for v in "$@"; do
  d=$S/$v
  grep -oE '^tests/golden/\S+\.velt( \[|:)' "$d/gold_disk.txt" | sed -E 's/( \[|:)$//' | sort -u > "$d/fail_files.txt"
  echo "$v: $(wc -l < "$d/fail_files.txt" | tr -d ' ') files"
  if [ -n "$prev" ]; then
    echo "  fixed since $prev: $(comm -23 "$S/$prev/fail_files.txt" "$d/fail_files.txt" | wc -l | tr -d ' ')"
    echo "  new since $prev:"
    comm -13 "$S/$prev/fail_files.txt" "$d/fail_files.txt" | sed 's/^/    /'
  fi
  prev=$v
done
