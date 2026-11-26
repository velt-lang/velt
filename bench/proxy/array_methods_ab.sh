#!/usr/bin/env bash
# What the records of array methods on a proxy cost (Linux, valgrind): instructions (cachegrind,
# VELT_THREADS=1, release compilers) at two git refs of
# - running bench/proxy/proxy_array_methods.velt, built --release by that ref's compiler;
# - compiling it (`velt build --release`), and compiling tests/golden/m1/hello.velt, a program
#   without proxies (its VIR is compared too: it should not change).
#
#   bench/proxy/array_methods_ab.sh WORKDIR BASE_REF HEAD_REF
#
# Each ref is exported to WORKDIR/src-<sha> and built into WORKDIR/target-<sha>. Keep WORKDIR on
# a Linux file system. Run it from inside the repository, or with GIT_DIR set.
set -uo pipefail
[ $# -ge 3 ] || { echo "usage: bench/proxy/array_methods_ab.sh WORKDIR BASE_REF HEAD_REF" >&2; exit 2; }
W=$1
mkdir -p "$W"
W=$(cd "$W" && pwd)
shas=()
for ref in "$2" "$3"; do
  s=$(git rev-parse --short=10 "$ref^{commit}") || exit 1
  shas+=("$s")
  if [ ! -d "$W/src-$s" ]; then
    mkdir -p "$W/src-$s.tmp"
    git archive "$s" | tar -x -C "$W/src-$s.tmp"
    find "$W/src-$s.tmp" -type f -exec touch {} +
    mv "$W/src-$s.tmp" "$W/src-$s"
  fi
  if [ ! -x "$W/target-$s/release/velt" ]; then
    (cd "$W/src-$s" && CARGO_TARGET_DIR="$W/target-$s" cargo build --release -q -p veltc --bin velt \
      && CARGO_TARGET_DIR="$W/target-$s" cargo build --release -q -p velt_rt -p velt_rt_shared) || exit 1
  fi
done
# count <sha> <program and args...>: the instructions of one run in that ref's tree.
count() {
  local s=$1
  shift
  (cd "$W/src-$s" && VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_STD="$W/src-$s/std" valgrind \
    --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null |
    grep -oP 'I\s+refs:\s+\K[\d,]+' | tr -d ,)
}
for s in "${shas[@]}"; do
  v="$W/target-$s/release/velt"
  b="$W/bin-$s"
  mkdir -p "$b"
  (cd "$W/src-$s" && VELT_STD="$W/src-$s/std" VELT_BUILD_CACHE=off "$v" build --release \
    bench/proxy/proxy_array_methods.velt -o "$b/methods") || exit 1
  (cd "$W/src-$s" && VELT_STD="$W/src-$s/std" VELT_BUILD_CACHE=off "$v" build --release --emit vir \
    tests/golden/m1/hello.velt > "$b/hello.vir") || exit 1
  echo "$s run methods $(count "$s" "$b/methods")"
  echo "$s build methods $(count "$s" "$v" build --release bench/proxy/proxy_array_methods.velt -o "$b/m2")"
  echo "$s build hello $(count "$s" "$v" build --release tests/golden/m1/hello.velt -o "$b/h2")"
  "$b/methods"
done
if cmp -s "$W/bin-${shas[0]}/hello.vir" "$W/bin-${shas[1]}/hello.vir"; then
  echo "hello VIR identical"
else
  echo "hello VIR differs"
fi
