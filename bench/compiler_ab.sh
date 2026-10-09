#!/usr/bin/env bash
# Compiler A/B (Linux): the instructions each benchmark executes when built by two commits'
# compilers, each with its own std and runtime. For changes to the compiler or std; for a change
# to `velt_rt` alone, bench/runtime_ab.sh holds the generated code fixed.
#
#   bench/compiler_ab.sh BASE_REF HEAD_REF [WORK]
#
# Exports both refs with `git archive` to WORK/base-src and WORK/branch-src, builds each tree's
# velt and runtime (release), then builds every benchmark in branch-src/bench (the same path in
# base-src) with each toolchain (LLVM, --release) and runs it under cachegrind with
# VELT_THREADS=1. Prints `name base-Ir branch-Ir same|DIFF` per program (DIFF: the outputs
# differ; db/velt/sqlite prints its timings, so it is DIFF unless those are stripped), then the
# compile row (`velt check bench/compile/all_std.vlt`), then the programs sorted by change.
# A program that fails to build prints FAIL. Knobs: JOBS (parallel programs, default 6), ONLY and
# SKIP (regular expressions over the paths), TO (timeout per run, seconds, default 1800).
# WORK defaults to $CARGO_TARGET_DIR/compiler-ab; keep it on a Linux file system (not /mnt/*).
# Needs: cargo, clang, valgrind, python3, git.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
BASE=${1:?base ref}
HEAD_REF=${2:?head ref}
W=${3:-${CARGO_TARGET_DIR:-$ROOT/target}/compiler-ab}
mkdir -p "$W/tmp" "$W/cg"
export TMPDIR=$W/tmp CARGO_INCREMENTAL=0

for v in base branch; do
  ref=$BASE; [[ $v == branch ]] && ref=$HEAD_REF
  rm -rf "$W/$v-src"; mkdir -p "$W/$v-src"
  git -C "$ROOT" archive "$ref" | tar -x -C "$W/$v-src"
  (cd "$W/$v-src" && CARGO_TARGET_DIR=$W/target-$v cargo build --release -q -p veltc -p velt_rt)
  mkdir -p "$W/tc-$v"
  cp "$W/target-$v/release/velt" "$W/target-$v/release/libvelt_rt.a" "$W/tc-$v/"
done

OUT=$W/cg
one() {
  src=$1; rel=${src#$W/branch-src/bench/}; name=$(echo "$rel" | sed 's#/#__#g; s#\.vlt$##'); res="$rel"
  for v in base branch; do
    exe=$OUT/$name-$v
    s=$W/$v-src/bench/$rel
    if ! VELT_STD=$W/$v-src/std VELT_RT_LIB=$W/tc-$v/libvelt_rt.a "$W/tc-$v/velt" build --release --backend llvm "$s" -o "$exe" > "$exe.build" 2>&1; then res="$res FAIL"; continue; fi
    (cd "$OUT" && VELT_THREADS=1 timeout "${TO:-1800}" valgrind --vgdb=no --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$exe" < /dev/null > "$exe.out" 2> "$exe.vg") || true
    ir=$(grep -oP 'I\s+refs:\s+\K[0-9,]+' "$exe.vg" | tr -d ,) || true; res="$res ${ir:-NA}"
    rm -f "$exe" "$exe.o" "$exe.link-stamp"
  done
  same=same; cmp -s "$OUT/$name-base.out" "$OUT/$name-branch.out" || same=DIFF
  echo "$res $same"
}
export -f one; export W OUT
find "$W/branch-src/bench" -name '*.vlt' | grep -E "${ONLY:-.}" | grep -vE "${SKIP:-^$}" | sort \
  | xargs -P "${JOBS:-6}" -I{} bash -c 'one {}' | tee "$W/counts.txt"

for v in base branch; do
  ir=$(cd "$W" && VELT_STD=$W/$v-src/std valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$W/tc-$v/velt" check "$W/$v-src/bench/compile/all_std.vlt" 2>&1 >/dev/null | grep -oP 'I\s+refs:\s+\K[0-9,]+' | tr -d ,)
  echo "compile $v $ir"
done

python3 - "$W/counts.txt" <<'EOF'
import sys
rows = []
for line in open(sys.argv[1]):
    p = line.split()
    if len(p) == 4 and p[1].isdigit() and p[2].isdigit():
        b, c = int(p[1]), int(p[2])
        rows.append((p[0], b, c, (c - b) / b * 100, p[3]))
for r in sorted(rows, key=lambda r: -r[3]):
    print(f"{r[0]:45} {r[1]:>15,} {r[2]:>15,} {r[3]:+7.2f}% {r[4]}")
print(len(rows), "measured")
EOF
