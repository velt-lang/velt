#!/bin/bash
set -eu
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/4d96d609-2239-43e8-bcbb-b3568d1a3979/scratchpad
VAR=/Users/andii/dev/velt/branches/sleep-combo-var
for v in lazyw fired noexp nohome; do
  d=$VAR/a-$v
  if [ -d "$d" ]; then find "$d" -type f -delete; fi
  mkdir -p "$d"
  cp -R "$VAR/all1/std" "$d/std"
  python3 "$S/cut2.py" "$d/std" "$v"
  echo "made a-$v"
done
