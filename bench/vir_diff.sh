#!/usr/bin/env bash
# The VIR two velt binaries emit for every benchmark, compared (a change that must leave
# programs as they were, such as weak references for programs that use none):
#
#   bench/vir_diff.sh BASE_VELT HEAD_VELT [WORK_DIR]
#
# Runs `velt build --emit vir` on each bench/**/*.vlt (except `_` modules) with both binaries,
# each from its own tree's standard library when the binary sits in a checkout's target
# directory, and prints one line per program that differs, then a summary. Exits 1 if any
# differs. A program that names `WeakMap`, `WeakSet` or `WeakRef` is listed but not counted.
set -uo pipefail
if [ $# -lt 2 ]; then
  echo "usage: bench/vir_diff.sh BASE_VELT HEAD_VELT [WORK_DIR]" >&2
  exit 2
fi
BASE=$1
HEAD=$2
WORK=${3:-$(mktemp -d)}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$WORK"
same=0
differ=0
skipped=0
while IFS= read -r src; do
  rel=${src#"$ROOT/"}
  name=$(echo "$rel" | tr '/' '_')
  "$BASE" build "$src" --emit vir > "$WORK/$name.base" 2>&1
  "$HEAD" build "$src" --emit vir > "$WORK/$name.head" 2>&1
  if grep -qE 'WeakMap|WeakSet|WeakRef' "$src"; then
    echo "uses weak references: $rel"
    skipped=$((skipped + 1))
  elif cmp -s "$WORK/$name.base" "$WORK/$name.head"; then
    same=$((same + 1))
  else
    echo "DIFFERS: $rel"
    differ=$((differ + 1))
  fi
done < <(find "$ROOT/bench" -name '*.vlt' ! -name '_*' | sort)
echo "identical: $same, differ: $differ, use weak references: $skipped"
[ "$differ" -eq 0 ]
