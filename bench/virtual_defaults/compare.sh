#!/bin/bash
# Virtual calls of methods whose overrides have other defaults (#1506): each program built with
# two compilers, then run alternately, pinned to one core; each run prints its loop times in ms.
# usage: BASE=<velt> HEAD=<velt> bench/virtual_defaults/compare.sh [rounds]
#   vd        a trailing argument left out           vd_full  every argument passed
#   vd_same   overrides with the same default        iface    through an interface, then `undefined`
#   undef     `m(i, undefined)`                      if1      through an interface only
#   pick      a value that may be `undefined` (the general fill entry)
set -e
dir=$(cd "$(dirname "$0")" && pwd)
out=${OUT:-$(mktemp -d)}
for f in vd vd_full vd_same iface undef if1 pick; do
  "$BASE" build --release "$dir/$f.vlt" -o "$out/$f.base" > /dev/null
  "$HEAD" build --release "$dir/$f.vlt" -o "$out/$f.head" > /dev/null
done
for r in $(seq "${1:-8}"); do
  for f in vd vd_full vd_same iface undef if1 pick; do
    for v in base head; do echo "$f $v $(taskset -c "${CORE:-1}" "$out/$f.$v" | cut -d' ' -f3-)"; done
  done
done
