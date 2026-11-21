#!/bin/bash
# Std trees for the VM: the combination (#3273 + #3322) and each cut on top of it.
set -eu
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/4d96d609-2239-43e8-bcbb-b3568d1a3979/scratchpad
SRC=/Users/andii/dev/velt/branches/sleep-combo/std
VAR=/Users/andii/dev/velt/branches/sleep-combo-var
mk() { # <name> <cuts...>
  name=$1; shift
  d=$VAR/$name
  if [ -d "$d" ]; then find "$d" -type f -delete; fi
  mkdir -p "$d"
  cp -R "$SRC" "$d/std"
  for c in "$@"; do python3 "$S/cut.py" "$d/std" "$c"; done
  echo "made $name"
}
mk combo
mk park park
mk init init
mk drop drop
mk dropw dropw
mk all1 park init drop
mk all2 park init dropw
