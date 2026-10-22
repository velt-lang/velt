#!/usr/bin/env bash
# What `Proxy` / `Reflect` cost the compiler (Linux, valgrind): instructions of `velt check` and
# `velt build --emit vir` at two git refs (cachegrind, VELT_THREADS=1, release builds), counted
# as bench/nightly.sh counts them, for a program that never names `Proxy` (tests/golden/m1/hello,
# examples/http_hello), for bench/compile/all_std.vlt (with its code lines of std/, the nightly
# rule's per-line comparison), and at HEAD_REF for all_std.vlt without the lines naming `Proxy`
# and `Reflect` (so their modules are not loaded) and with only a comment naming `Proxy` (loaded,
# not used): the difference is what checking std/proxy*.vlt costs.
#
#   bench/proxy/load_cost.sh WORKDIR BASE_REF HEAD_REF
#
# Each ref is exported to WORKDIR/src-<sha> and built into WORKDIR/target. Keep WORKDIR on a
# Linux file system. Run it from inside the repository, or with GIT_DIR set.
set -uo pipefail
[ $# -ge 3 ] || { echo "usage: bench/proxy/load_cost.sh WORKDIR BASE_REF HEAD_REF" >&2; exit 2; }
W=$1
mkdir -p "$W"
W=$(cd "$W" && pwd)
shas=()
for ref in "$2" "$3"; do
  s=$(git rev-parse --short=8 "$ref^{commit}") || exit 1
  shas+=("$s")
  if [ ! -d "$W/src-$s" ]; then
    mkdir -p "$W/src-$s.tmp"
    git archive "$s" | tar -x -C "$W/src-$s.tmp"
    find "$W/src-$s.tmp" -type f -exec touch {} +
    mv "$W/src-$s.tmp" "$W/src-$s"
  fi
  if [ ! -x "$W/velt-$s" ]; then
    (cd "$W/src-$s" && CARGO_TARGET_DIR="$W/target" cargo build --release -q -p veltc) || exit 1
    cp "$W/target/release/velt" "$W/velt-$s"
  fi
done
# count <sha> <velt args...>: the instructions of one run in that ref's tree, with its std.
count() {
  local s=$1
  shift
  (cd "$W/src-$s" && VELT_THREADS=1 VELT_STD="$W/src-$s/std" valgrind --tool=cachegrind \
    --cache-sim=no --cachegrind-out-file=/dev/null "$W/velt-$s" "$@" 2>&1 >/dev/null |
    grep -oP 'I\s+refs:\s+\K[\d,]+' | tr -d ,)
}
lines() {
  find "$W/src-$1/std" -name '*.vlt' -exec cat {} + | awk '
    block { if (/\*\//) block = 0; next }
    /^[ \t]*(\/\/|$)/ { next }
    /^[ \t]*\/\*/ { if (!/\*\//) block = 1; next }
    { n++ }
    END { print n + 0 }'
}
for s in "${shas[@]}"; do
  echo "$s std code lines $(lines "$s")"
  echo "$s check all_std $(count "$s" check bench/compile/all_std.vlt)"
  echo "$s check m1/hello $(count "$s" check tests/golden/m1/hello.vlt)"
  echo "$s check http_hello $(count "$s" check examples/http_hello.vlt)"
  echo "$s vir http_hello $(count "$s" build --release --emit vir examples/http_hello.vlt)"
done
h=${shas[1]}
c="$W/src-$h/bench/compile"
grep -v "Proxy\|Reflect" "$c/all_std.vlt" > "$c/all_std_no_proxy.vlt"
{ grep -v "new Proxy\|Reflect\." "$c/all_std.vlt"; echo "// Proxy"; } > "$c/all_std_proxy_named.vlt"
for v in all_std_no_proxy all_std_proxy_named; do
  echo "$h check $v $(count "$h" check "bench/compile/$v.vlt")"
done
