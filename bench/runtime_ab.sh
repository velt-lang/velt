#!/usr/bin/env bash
# Runtime A/B (Linux): the instructions each benchmark executes when the same program, built by
# this checkout's compiler, is linked against another commit's runtime (A, default origin/main)
# and against this checkout's runtime (B). For a change to `velt_rt` alone: the generated code is
# identical, so the difference is the runtime's. Counts come from valgrind's cachegrind, as in
# bench/nightly.sh, and repeat to within 0.1%.
#
#   bench/runtime_ab.sh [--base REF] [--work DIR] [--jobs N] [FILTER]
#
# Exports REF with `git archive` to WORK/base-src and builds its runtime (release) there, builds
# this checkout's velt and runtime (release), then for every benchmark whose name matches the
# regular expression FILTER builds it twice (LLVM, --release, `VELT_RT_LIB` naming each runtime)
# and runs each under cachegrind with VELT_THREADS=1. Writes WORK/counts.tsv (name, A
# instructions, B instructions, whether the two outputs are the same) and the raw output of every
# run to WORK/runs/, and prints a Markdown table. WORK defaults to $CARGO_TARGET_DIR/runtime-ab.
# Needs: cargo, clang, valgrind, python3, git.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
PYTHON=$(command -v python3 || command -v python)
BASE=origin/main
WORK=
JOBS=4
FILTER=.
while [ $# -gt 0 ]; do
  case "$1" in
    --base) BASE=$2; shift 2 ;;
    --work) WORK=$2; shift 2 ;;
    --jobs) JOBS=$2; shift 2 ;;
    -*) echo "usage: bench/runtime_ab.sh [--base REF] [--work DIR] [--jobs N] [FILTER]" >&2; exit 2 ;;
    *) FILTER=$1; shift ;;
  esac
done
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
WORK=${WORK:-$TARGET/runtime-ab}
mkdir -p "$WORK/runs"
WORK=$(cd "$WORK" && pwd)

BASE_SHA=$(git -C "$ROOT" rev-parse "$BASE^{commit}")
echo "A: runtime of $BASE ($BASE_SHA)" >&2
echo "B: runtime of this checkout ($(git -C "$ROOT" rev-parse HEAD)$(git -C "$ROOT" diff --quiet HEAD -- crates || echo ', with uncommitted changes'))" >&2

rm -rf "$WORK/base-src"
mkdir -p "$WORK/base-src"
git -C "$ROOT" archive "$BASE_SHA" | tar -x -C "$WORK/base-src"
echo "building A's runtime (release)..." >&2
(cd "$WORK/base-src" && CARGO_TARGET_DIR="$WORK/base-target" cargo build --release -q -p velt_rt)
cp "$WORK/base-target/release/libvelt_rt.a" "$WORK/rt-a.a"
echo "building velt and B's runtime (release)..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
cp "$TARGET/release/libvelt_rt.a" "$WORK/rt-b.a"
VELT="$TARGET/release/velt"

# name|source|arguments: the string-heavy benchmarks, the ones a string runtime change can move.
benches() {
  local b=$ROOT/bench f rel
  for f in "$b"/strings.vlt "$b"/sort.vlt "$b"/hashmap.vlt "$b"/strings_utf16/*.vlt "$b"/json/*.vlt; do
    rel=${f#"$b/"}
    echo "${rel%.vlt}|$f|"
  done
  for n in strings tokenize wordcount keys record sortcmp; do
    echo "typical/$n|$b/typical/$n.vlt|"
  done
  echo "typical/consume 1000000|$b/typical/consume.vlt|1000000"
}

# one "name|source|arguments": prints "name\tA\tB\tsame|DIFF".
one() {
  local name src args key v exe ir res
  IFS='|' read -r name src args <<< "$1"
  key=$(echo "$name" | tr -c 'a-zA-Z0-9_\n' '_')
  res=$name
  for v in a b; do
    exe=$WORK/runs/$key-$v
    if ! VELT_STD=$ROOT/std VELT_RT_LIB=$WORK/rt-$v.a "$VELT" build --release --backend llvm "$src" -o "$exe" > "$exe.build" 2>&1; then
      echo "$name: build against runtime $v failed, see $exe.build" >&2
      return 1
    fi
    # shellcheck disable=SC2086 # the arguments are words
    VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
      "$exe" $args < /dev/null > "$exe.out" 2> "$exe.vg"
    ir=$(sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' "$exe.vg" | tr -d ,)
    [ -n "$ir" ] || { echo "$name: no count from cachegrind, see $exe.vg" >&2; return 1; }
    res="$res"$'\t'"$ir"
  done
  if cmp -s "$WORK/runs/$key-a.out" "$WORK/runs/$key-b.out"; then res="$res"$'\tsame'; else res="$res"$'\tDIFF'; fi
  echo "$res"
}
export -f one
export WORK ROOT VELT

benches | grep -E -- "$FILTER" | xargs -d '\n' -P "$JOBS" -I{} bash -c 'one "$@"' _ {} > "$WORK/counts.tsv"

"$PYTHON" - "$WORK/counts.tsv" "$BASE" <<'EOF'
import sys
rows = sorted(line.rstrip("\n").split("\t") for line in open(sys.argv[1]))
print(f"| benchmark | A: {sys.argv[2]} (Ir) | B: this checkout (Ir) | change | output |")
print("|---|---:|---:|---:|---|")
for name, a, b, same in rows:
    a, b = int(a), int(b)
    print(f"| {name} | {a:,} | {b:,} | {100 * (b - a) / a:+.2f}% | {same} |")
EOF
