#!/usr/bin/env bash
# The std identity oracle over many programs (#2943 phase 0): which std definitions are checked
# differently in a program than in `velt check --std`. Separate compilation needs none.
#
#   bench/compile/std_identity.sh VELT WORK [PROGRAM...]
#
# Without PROGRAMs: every golden with a `.out` (tests/golden). Runs up to $JOBS (default 3) checks
# at once, each writing its std definitions' hashes (VELT_STD_SUMMARY), then prints
# bench/compile/std_identity.py's report and exits with its status.
set -euo pipefail
VELT=$(realpath "$1"); WORK=$2; shift 2
HERE=$(cd "$(dirname "$0")" && pwd); ROOT=$(cd "$HERE/../.." && pwd)
mkdir -p "$WORK/programs"; WORK=$(realpath "$WORK")
rm -f "$WORK"/programs/*.tsv "$WORK/reference.tsv"
VELT_STD_SUMMARY="$WORK/reference.tsv" "$VELT" check --std >/dev/null
if [ $# -eq 0 ]; then
  set -- $(cd "$ROOT" && find tests/golden -name '*.velt' | while read -r f; do
    [ -f "${f%.velt}.out" ] && echo "$ROOT/$f"; done | sort)
fi
export VELT WORK
printf '%s\n' "$@" | xargs -P "${JOBS:-3}" -I{} bash -c '
  f={}; out="$WORK/programs/$(echo "$f" | tr "/" "_").tsv"
  VELT_STD_SUMMARY="$out" "$VELT" check "$f" >/dev/null 2>&1 || true'
python3 "$HERE/std_identity.py" "$WORK/reference.tsv" "$WORK"/programs/*.tsv
