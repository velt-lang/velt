#!/usr/bin/env bash
# Identity and hashing on a proxiable object type (#1485): proxy_identity_keys.vlt (`Map` and
# `Set` keyed by an object type), proxy_identity_index.vlt (`indexOf` / `includes` over an
# array of it) and proxy_view_convert.vlt (conversions that make views), each built by two
# compilers (LLVM release, with the release runtime `$VELT_RT_LIB` when set) and run
# interleaved RUNS times. Prints the range of user seconds per program and compiler, and stops
# when the two builds print different output.
#
#   bench/proxy/proxy_identity.sh <base velt> <base std> <head velt> <head std> [runs] [workdir]
set -euo pipefail
[ $# -ge 4 ] || { echo "usage: bench/proxy/proxy_identity.sh BASE_VELT BASE_STD HEAD_VELT HEAD_STD [runs] [workdir]" >&2; exit 2; }
HERE=$(cd "$(dirname "$0")" && pwd)
RUNS=${5:-9}
W=${6:-$(mktemp -d)}
mkdir -p "$W"
TIMEFORMAT=%U
for p in proxy_identity_keys proxy_identity_index proxy_view_convert; do
  VELT_STD="$2" "$1" build --release --backend llvm "$HERE/$p.vlt" -o "$W/$p-base" >/dev/null
  VELT_STD="$4" "$3" build --release --backend llvm "$HERE/$p.vlt" -o "$W/$p-head" >/dev/null
  # A compiler from before views (#1485) stops proxy_view_convert with a TypeError.
  if ! b=$("$W/$p-base" 2>/dev/null); then
    echo "$p: skipped, the base build has no views"
    continue
  fi
  h=$("$W/$p-head")
  if [ "$b" != "$h" ]; then
    echo "$p: base printed '$b', head '$h'" >&2
    exit 1
  fi
  : > "$W/$p-base.t"; : > "$W/$p-head.t"
  for _ in $(seq "$RUNS"); do
    for side in base head; do
      { time "$W/$p-$side" > /dev/null; } 2>> "$W/$p-$side.t"
    done
  done
  for side in base head; do
    sort -n "$W/$p-$side.t" | awk -v p="$p" -v s="$side" \
      "{ v[NR] = \$1 } END { printf \"%s %s: %s to %s s user, median %s (%d runs)\n\", p, s, v[1], v[NR], v[int((NR + 1) / 2)], NR }"
  done
done
uptime
