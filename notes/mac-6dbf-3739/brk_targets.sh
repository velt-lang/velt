#!/bin/bash
# Dry run of brk3687r.sh's swaps: each target must occur exactly once in the worktree's std.
S="/private/tmp/claude-501/-Users-andii-dev-velt-main/6dbf5d3c-1862-4ea4-a154-094b5a8b6b53/scratchpad"
W="$HOME/dev/velt/branches/root-turns-3687-r1"
O="$S/brkdry"
rm -rf "$O" && mkdir -p "$O" && cp -R "$W/std" "$O/std"
# Re-define brk to only swap (on a fresh copy each time) and report.
sed -n '/^brk "/,$p' "$S/brk3687r.sh" > "$O/calls.sh"
brk() {
  cp "$W/std/$2" "$O/f.velt"
  if python3 "$S/swap.py" "$O/f.velt" "$3" "$4" > /dev/null 2> "$O/err"; then
    echo "ok: $1"
  else
    echo "TARGET BAD: $1: $(cat "$O/err")"
  fi
}
source "$O/calls.sh"
rm -rf "$O"
