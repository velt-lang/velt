#!/usr/bin/env bash
# The std identity oracle over many programs (#2943 phase 0): which std definitions are checked
# differently in a program than in `velt check --std`. Separate compilation needs none.
#
#   bench/compile/std_identity.sh VELT WORK [PROGRAM...]
#
# Without PROGRAMs: every golden with a `.out` (tests/golden). Runs up to $JOBS (default 3) checks
# at once, each writing its std definitions' hashes (VELT_STD_SUMMARY), names the programs that
# failed to check (on stderr), then prints bench/compile/std_identity.py's report and exits with
# its status. With VELT_STD_SUMMARY_EAGER=1 in the environment, both sides check all of std.
set -euo pipefail
VELT=$(realpath "$1"); WORK=$2; shift 2
HERE=$(cd "$(dirname "$0")" && pwd); ROOT=$(cd "$HERE/../.." && pwd)
mkdir -p "$WORK/programs"; WORK=$(realpath "$WORK")
rm -f "$WORK"/programs/*.tsv "$WORK/reference.tsv" "$WORK/failed.txt"
VELT_STD_SUMMARY="$WORK/reference.tsv" "$VELT" check --std >/dev/null
if [ $# -eq 0 ]; then
  (cd "$ROOT" && find tests/golden -name '*.velt' -print0) | sort -z | while IFS= read -r -d '' f; do
    [ -f "$ROOT/${f%.velt}.out" ] && printf '%s\0' "$ROOT/$f"
  done > "$WORK/programs.list"
else
  printf '%s\0' "$@" > "$WORK/programs.list"
fi
export VELT WORK
# A program that fails to check writes no summary: it is counted and named, not compared.
xargs -0 -P "${JOBS:-3}" -n 1 bash -c '
  out="$WORK/programs/$(printf "%s" "$1" | tr "/" "_").tsv"
  VELT_STD_SUMMARY="$out" "$VELT" check "$1" >/dev/null 2>&1 || echo "$1" >> "$WORK/failed.txt"' _ \
  < "$WORK/programs.list"
if [ -s "$WORK/failed.txt" ]; then
  echo "$(wc -l < "$WORK/failed.txt" | tr -d " ") programs failed to check (not compared):" >&2
  sed 's/^/  /' "$WORK/failed.txt" >&2
fi
python3 "$HERE/std_identity.py" "$WORK/reference.tsv" "$WORK"/programs/*.tsv
