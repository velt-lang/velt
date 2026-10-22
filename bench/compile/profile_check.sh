#!/usr/bin/env bash
# Where `velt check bench/compile/all_std.vlt` spends its instructions at two git refs: runs it
# under callgrind for each (VELT_THREADS=1, release build with line tables) and prints the
# functions whose inclusive and exclusive counts changed most. For finding which sema pass a
# change to std/ or to the compiler makes slower; bench/compile/count_check.sh gives the plain
# totals the way bench/nightly.sh counts them.
#
#   bench/compile/profile_check.sh WORKDIR BASE_REF HEAD_REF [FILE]
#
# FILE (default bench/compile/all_std.vlt, from each ref's tree) is what `velt check` checks.
# Each ref is exported to WORKDIR/src-<sha> and built into WORKDIR/target-<sha>. The callgrind
# outputs stay in WORKDIR/cg-<sha>.out for callgrind_annotate. Keep WORKDIR on a Linux file
# system (not /mnt/*). Linux only. Needs: git, tar, cargo, valgrind, python3.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
[ $# -ge 3 ] || { echo "usage: bench/compile/profile_check.sh WORKDIR BASE_REF HEAD_REF [FILE]" >&2; exit 2; }
WORK=$1; BASE=$2; HEAD_REF=$3; FILE=${4:-bench/compile/all_std.vlt}
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
shas=()
for ref in "$BASE" "$HEAD_REF"; do
  sha=$(git -C "$ROOT" rev-parse --short=8 "$ref^{commit}")
  shas+=("$sha")
  src="$WORK/src-$sha"
  if [ ! -d "$src" ]; then
    mkdir -p "$src.tmp"
    git -C "$ROOT" archive "$sha" | tar -x -C "$src.tmp"
    find "$src.tmp" -type f -exec touch {} +
    mv "$src.tmp" "$src"
  fi
  (cd "$src" && CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_TARGET_DIR="$WORK/target-$sha" \
    cargo build --release -q -p veltc) >&2
  (cd "$WORK" && VELT_THREADS=1 VELT_STD="$src/std" valgrind --tool=callgrind \
    --callgrind-out-file="$WORK/cg-$sha.out" "$WORK/target-$sha/release/velt" check "$src/$FILE" \
    > /dev/null 2> "$WORK/cg-$sha.log")
  # Code lines of std/, counted as bench/nightly.sh counts them.
  lines=$(find "$src/std" -name '*.vlt' -exec cat {} + | awk '
    block { if (/\*\//) block = 0; next }
    /^[ 	]*(\/\/|$)/ { next }
    /^[ 	]*\/\*/ { if (!/\*\//) block = 1; next }
    { n++ }
    END { print n + 0 }')
  total=$(grep -oP 'Collected : \K[0-9]+' "$WORK/cg-$sha.log")
  echo "$sha total $total, $lines code lines of std, $((total / lines)) per line"
  callgrind_annotate --inclusive=yes "$WORK/cg-$sha.out" > "$WORK/incl-$sha.txt"
  callgrind_annotate "$WORK/cg-$sha.out" > "$WORK/excl-$sha.txt"
done

python3 - "$WORK" "${shas[0]}" "${shas[1]}" <<'EOF'
import re, sys
work, a, b = sys.argv[1:]
def load(path, inclusive):
    rows = {}
    for line in open(path, errors="replace"):
        m = re.match(r"\s*([\d,]+)\s+(?:\([^)]*\)\s+)?(.*?)\s*$", line)
        if not m or ":" not in m.group(2) or m.group(2).startswith("=>"):
            continue
        # The same function at both refs: without the tree's path and the recursion depth.
        fn = re.sub(r"^.*/src-[0-9a-f]+/", "", m.group(2).split(" [")[0])
        # A recursive call's inclusive count is in its outermost frame already.
        if re.search(r"'\d+$", fn):
            if inclusive:
                continue
            fn = re.sub(r"'\d+$", "", fn)
        rows[fn] = rows.get(fn, 0) + int(m.group(1).replace(",", ""))
    return rows
for kind in ("incl", "excl"):
    inc = kind == "incl"
    x, y = load(f"{work}/{kind}-{a}.txt", inc), load(f"{work}/{kind}-{b}.txt", inc)
    diff = sorted(((y.get(k, 0) - x.get(k, 0), k) for k in set(x) | set(y)), reverse=True)
    print(f"\n{'inclusive' if inc else 'exclusive'}, largest growth ({a} -> {b}):")
    for d, k in diff[:40]:
        print(f"{d:>14,} {x.get(k, 0):>14,} {y.get(k, 0):>14,}  {k[:150]}")
EOF
