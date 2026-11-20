#!/bin/bash
# u4_one.sh <golden path under tests/golden>: velt check of one golden in branches/d1-open-pr with
# VELT_UNITS=2, without and then with std's unit on disk (a fresh dir: first saves, then loads),
# printing each one's stderr minus the units report lines. Uses the velt built last (U4 draft).
W=/Users/andii/dev/velt/branches/d1-open-pr
g="$W/tests/golden/$1"
disk=$(mktemp -d /tmp/claude-u4one.XXXX)
export VELT_STD=/private/tmp/claude-501/-Users-andii-dev-velt-main/9a2100e7-8c37-4a77-b4d9-1d2c2a1dfa37/scratchpad/u4-std/std
cd "$(dirname "$g")" || exit 2
for mode in plain save load; do
  echo "== $mode"
  case $mode in
    plain) env -u VELT_UNITS_DISK VELT_UNITS=2 "$W/target/debug/velt" check "$(basename "$g")" 2>&1 ;;
    *) VELT_UNITS=2 VELT_UNITS_DISK=$disk "$W/target/debug/velt" check "$(basename "$g")" 2>&1 ;;
  esac | grep -v "^velt-units" | head -${LINES:-14}
done
rm -rf "$disk"
