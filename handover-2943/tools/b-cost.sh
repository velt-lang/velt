#!/bin/bash
# b-cost.sh: B's gain. An installed-style prefix (<tmp>/toolchains/0.0.0/{bin/velt,std}) with
# this worktree's velt and a copy of its std; warm `velt check` instructions of hello by units
# (median of 3 after a warm-up), std writable (stat check) vs read-only (no stat check).
R=/Users/andii/dev/velt/branches/d1-open-pr
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad
P=$S/fake-root/toolchains/0.0.0
U=$S/b-units
if [ -d "$P/std" ]; then chmod -R u+w "$P/std"; fi
rm -rf "$S/fake-root" "$U"
mkdir -p "$P/bin" "$U"
cp "$R/target/debug/velt" "$P/bin/velt"
cp -R "$R/std" "$P/std"
unset VELT_STD
export VELT_THREADS=1 VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_DISK=$U
cd "$S/ab/examples" || exit 1
for mode in writable read-only; do
  case $mode in
    writable) find "$P/std" -type d -exec chmod 755 {} + ; find "$P/std" -type f -exec chmod 644 {} + ;;
    read-only) find "$P/std" -type f -exec chmod 444 {} + ; find "$P/std" -type d -exec chmod 555 {} + ;;
  esac
  vals=()
  for i in 1 2 3 4; do
    echo "const zb$RANDOM$i = 1;" >> hello.velt
    log=$(/usr/bin/time -l "$P/bin/velt" check hello.velt 2>&1)
    echo "$log" | grep -v '^velt' | grep -qE "error|panicked|ICE" && echo "FAILED: $mode run $i: $(echo "$log" | grep -m1 -E 'error|panicked|ICE')" >&2
    [ "$i" = 1 ] && continue
    vals+=("$(echo "$log" | awk '/instructions retired/ {print $1}')")
  done
  med=$(printf '%s\n' "${vals[@]}" | sort -n | sed -n 2p)
  printf 'check hello units, std %-9s %6.2f G\n' "$mode" "$(echo "$med / 1000000000" | bc -l)"
done
# Leave it removable.
chmod -R u+w "$P/std"
