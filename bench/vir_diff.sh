#!/usr/bin/env bash
# The VIR two velt binaries emit for every benchmark, compared (a change that must leave
# programs as they were, such as weak references for programs that use none):
#
#   bench/vir_diff.sh BASE_VELT HEAD_VELT [WORK_DIR]
#
# Runs `velt build --emit vir` on each bench/**/*.vlt (except `_` modules) with both binaries
# and prints one line per program whose VIR differs, then a summary; exits 1 if any differs. A
# program that names `WeakMap`, `WeakSet` or `WeakRef` is listed but not compared. Before
# comparing, what only numbers definitions is normalized: the path of the tree each binary
# reads its standard library from, module numbers in mangled names (`_L21_`), the definition
# numbers in glue names (`_Gunclaimed_251`) and the class id at the start of a vtable static, so
# a definition added to the prelude does not count as a change.
set -uo pipefail
if [ $# -lt 2 ]; then
  echo "usage: bench/vir_diff.sh BASE_VELT HEAD_VELT [WORK_DIR]" >&2
  exit 2
fi
BASE=$1
HEAD=$2
WORK=${3:-$(mktemp -d)}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PYTHON=${PYTHON:-python3}
mkdir -p "$WORK"

# normalize <file>: the VIR with definition numbers replaced by `#`.
normalize() {
  "$PYTHON" - "$1" <<'EOF'
import re, sys
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    line = re.sub(r'[^ "]*/crates/veltc/\.\./\.\./std/', "<std>/", line)
    line = re.sub(r"_L\d+_", "_L#_", line)
    line = re.sub(r"_Gunclaimed_\d+", "_Gunclaimed_#", line)
    line = re.sub(r'^(static#\d+ align=8 ")(\\u\{[0-9a-f]+\}|[^\\])', r"\1#", line)
    sys.stdout.write(line)
EOF
}

same=0
differ=0
skipped=0
while IFS= read -r src; do
  rel=${src#"$ROOT/"}
  name=$(echo "$rel" | tr '/' '_')
  if grep -qE 'WeakMap|WeakSet|WeakRef' "$src"; then
    echo "uses weak references: $rel"
    skipped=$((skipped + 1))
    continue
  fi
  "$BASE" build "$src" --emit vir > "$WORK/$name.base.raw" 2>&1
  "$HEAD" build "$src" --emit vir > "$WORK/$name.head.raw" 2>&1
  normalize "$WORK/$name.base.raw" > "$WORK/$name.base"
  normalize "$WORK/$name.head.raw" > "$WORK/$name.head"
  if cmp -s "$WORK/$name.base" "$WORK/$name.head"; then
    same=$((same + 1))
  else
    echo "DIFFERS: $rel"
    differ=$((differ + 1))
  fi
done < <(find "$ROOT/bench" -name '*.vlt' ! -name '_*' | sort)
echo "identical: $same, differ: $differ, use weak references: $skipped"
[ "$differ" -eq 0 ]
