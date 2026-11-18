#!/bin/bash
# #3173's compile-time and size check: release builds with main's velt (the fix's parent) and the
# fix's velt (both release, same std = the fix branch's), build cache off, interleaved ROUNDS
# times: actors, shop-api and a std test (wall seconds, best of ROUNDS), and the binary size of
# hello and actors.
#   orb -m velt-prof bash notes/3097/vm-fix/compile-ab.sh [rounds]
set -u
source "$HOME/.cargo/env"
OUT=$HOME/b3097fix
WT=/Users/andii/dev/velt/branches/units-inline-import
rounds=${1:-3}
if [ ! -x "$OUT/tgt-main/release/velt" ]; then
  rm -rf "$OUT/src-main" && mkdir -p "$OUT/src-main"
  (cd "$WT" && git archive HEAD~1 | tar -x -C "$OUT/src-main")
  (cd "$OUT/src-main" && CARGO_TARGET_DIR=$OUT/tgt-main CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 \
    cargo build --release -q -p veltc --bin velt)
fi
declare -A V=([main]=$OUT/tgt-main/release/velt [units]=$OUT/tgt-units/release/velt)
RT=$OUT/tgt/release/libvelt_rt.a
export VELT_STD=$WT/std VELT_RT_LIB=$RT VELT_BUILD_CACHE=off
secs() { local TIMEFORMAT="%U %S"; { time "$@" >/dev/null 2>&1; } 2>"$OUT/cpu.txt" || echo "FAILED: $*" >&2; awk '{printf "%d", ($1 + $2) * 1000}' "$OUT/cpu.txt"; }
rm -f "$OUT"/sz-*
declare -A best=()
for r in $(seq 1 "$rounds"); do
  order="main units"; [ $((r % 2)) = 0 ] && order="units main"
  for s in $order; do
    v=${V[$s]}
    a=$(cd "$WT/examples/apps/actors" && secs "$v" build --release -o "$OUT/sz-actors-$s-$r")
    p=$(cd "$WT/examples/apps/shop-api" && secs "$v" build --release -o "$OUT/sz-shop-$s-$r")
    t=$(cd "$WT/examples/apps/seek" && secs "$v" build --release -o "$OUT/sz-seek-$s-$r")
    "$v" build --release "$WT/tests/golden/m1/hello.velt" -o "$OUT/sz-hello-$s-$r" >/dev/null 2>&1
    echo "round $r $s: actors ${a} ms cpu, shop-api ${p} ms cpu, seek ${t} ms cpu"
    for k in a p t; do
      cur=${!k}; key=$s-$k
      if [ -z "${best[$key]:-}" ] || [ "$cur" -lt "${best[$key]}" ]; then best[$key]=$cur; fi
    done
  done
done
for k in a p t; do
  awk -v n="$k" -v m="${best[main-$k]}" -v u="${best[units-$k]}" \
    'BEGIN { printf "best %s: main %d ms cpu, units %d ms cpu (%+.1f%%)\n", n, m, u, (u - m) * 100 / m }'
done
for b in hello actors; do
  m=$(stat -c %s "$OUT/sz-$b-main-1"); u=$(stat -c %s "$OUT/sz-$b-units-1")
  awk -v b="$b" -v m="$m" -v u="$u" 'BEGIN { printf "size %s: main %d, units %d (%+.2f%%)\n", b, m, u, (u - m) * 100 / m }'
done
