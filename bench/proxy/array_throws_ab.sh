#!/usr/bin/env bash
# What carrying errors out of the array methods on a proxy costs (Linux, valgrind): instructions
# (cachegrind, VELT_THREADS=1, release compilers) at two git refs of
# - the rows of bench/proxy/array_methods_ab.sh (running and compiling
#   bench/proxy/proxy_array_methods.velt, compiling tests/golden/m1/hello.velt, its VIR);
# - compiling bench/compile/all_std.velt (`velt build --release`, `velt check`), the generated
#   application of 10,000 lines (bench/compile/scale/app.py) and hello in debug;
# - running bench/proxy/proxy_get.ts (proxy and plain reads, two sizes) and
#   bench/proxy/proxy_signal.velt.
#
#   bench/proxy/array_throws_ab.sh WORKDIR BASE_REF HEAD_REF
#
# Give it a new WORKDIR: array_methods_ab.sh builds into fixed paths there, which a second run
# finds up to date. Keep WORKDIR on a Linux file system. Run it from inside the repository, or with GIT_DIR set.
set -uo pipefail
[ $# -ge 3 ] || { echo "usage: bench/proxy/array_throws_ab.sh WORKDIR BASE_REF HEAD_REF" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
bash "$here/array_methods_ab.sh" "$@" || exit 1
W=$(cd "$1" && pwd)
b=$(git rev-parse --short=10 "$2^{commit}") || exit 1
h=$(git rev-parse --short=10 "$3^{commit}") || exit 1
# count <sha> <program and args...>: the instructions of one run in that ref's tree.
count() {
  local s=$1
  shift
  (cd "$W/src-$s" && VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_STD="$W/src-$s/std" valgrind \
    --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null |
    grep -oP 'I\s+refs:\s+\K[\d,]+' | tr -d ,)
}
python3 "$W/src-$h/bench/compile/scale/app.py" 10000 "$W/app10k" > /dev/null || exit 1
for s in "$b" "$h"; do
  v="$W/target-$s/release/velt"
  # A fresh directory per run: `velt build` skips an output that is up to date.
  o=$(mktemp -d "$W/out-$s.XXXXXX") || exit 1
  echo "$s build all_std $(count "$s" "$v" build --release bench/compile/all_std.velt -o "$o/all_std")"
  echo "$s check all_std $(count "$s" "$v" check bench/compile/all_std.velt)"
  echo "$s build app10k $(count "$s" "$v" build --release "$W/app10k/main.velt" -o "$o/app10k")"
  echo "$s build hello-debug $(count "$s" "$v" build tests/golden/m1/hello.velt -o "$o/hd")"
  (cd "$W/src-$s" && VELT_STD="$W/src-$s/std" VELT_BUILD_CACHE=off "$v" build --release \
    bench/proxy/proxy_get.ts -o "$o/proxy_get") > /dev/null || exit 1
  for n in 100000 200000; do
    for mode in proxy plain; do
      echo "$s run proxy_get $mode $n $(count "$s" "$o/proxy_get" "$n" "$mode")"
    done
  done
  (cd "$W/src-$s" && VELT_STD="$W/src-$s/std" VELT_BUILD_CACHE=off "$v" build --release \
    bench/proxy/proxy_signal.velt -o "$o/proxy_signal") > /dev/null || exit 1
  echo "$s run proxy_signal $(count "$s" "$o/proxy_signal")"
done
