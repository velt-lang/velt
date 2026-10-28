#!/usr/bin/env bash
# Instructions `velt build` executes for one long `async main` against the same function without
# `await` (#1793), in two shapes:
# - calls (default): async_N.vlt makes N calls and awaits once;
# - awaits: async_N.vlt awaits N times, each value held to the end of `main`
#   (`const v<i> = "s" + (await Promise.resolve("x<i>"));` and a use of it), like a long script
#   with top-level await.
# sync_N.vlt is the same file with a synchronous `main` (and the awaited values computed in
# place). Prints one row per N (instructions in millions: async, sync, and the extra for async).
# With --spill it also runs callgrind and prints the instructions of the spill pass
# (`velt_vir::lower::async_fn::spill::spill`, inclusive) for the async file.
# With --check, exits 1 when the extra at the largest N is more than FACTOR times the whole sync
# build (lowering an async function must stay linear in its size, like a sync one).
# Counted with cachegrind / callgrind (VELT_THREADS=1), so the numbers do not depend on machine
# load. Linux only. A velt binary copied out of its target directory needs VELT_STD and
# VELT_RT_LIB.
#
#   bench/compile/async_long.sh [--shape calls|awaits] [--spill] [--check FACTOR] VELT WORKDIR [N...]
#
# N defaults to 250 500 1000 2000 4000 (calls) or 25 50 100 200 400 (awaits). Needs: valgrind.
set -euo pipefail
CHECK= SHAPE=calls SPILL=
while [ $# -gt 0 ]; do
  case $1 in
    --check) CHECK=$2; shift 2 ;;
    --shape) SHAPE=$2; shift 2 ;;
    --spill) SPILL=1; shift ;;
    *) break ;;
  esac
done
[ $# -ge 2 ] || { echo "usage: bench/compile/async_long.sh [--shape calls|awaits] [--spill] [--check FACTOR] VELT WORKDIR [N...]" >&2; exit 2; }
VELT=$1; WORK=$2; shift 2
SIZES=("$@")
if [ ${#SIZES[@]} -eq 0 ]; then
  if [ "$SHAPE" = awaits ]; then SIZES=(25 50 100 200 400); else SIZES=(250 500 1000 2000 4000); fi
fi
mkdir -p "$WORK"

program() { # kind n
  if [ "$SHAPE" = awaits ]; then
    if [ "$1" = async ]; then echo 'async function main() {'; else echo 'function main() {'; fi
    for ((i = 1; i <= $2; i++)); do
      if [ "$1" = async ]; then
        echo "  const v$i = \"s\" + (await Promise.resolve(\"x$i\"));"
      else
        echo "  const v$i = \"s\" + \"x$i\";"
      fi
      echo "  if (v$i.length < 0) console.log(v$i);"
    done
    echo "  console.log(v1, v$2);"
    echo '}'
    return
  fi
  echo 'function f(name: string, n: number) { if (n < 0) { console.log(name); } }'
  if [ "$1" = async ]; then echo 'async function main() {'; else echo 'function main() {'; fi
  for ((i = 1; i <= $2; i++)); do echo "  f(\"t$i\", $i);"; done
  if [ "$1" = async ]; then echo '  await Promise.resolve(1);'; fi
  echo '}'
}

count() { # file -> instructions
  local log="$WORK/$(basename "$1" .vlt).cg.log"
  VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
    "$VELT" build "$1" -o "$WORK/$(basename "$1" .vlt)" > /dev/null 2> "$log" ||
    { echo "velt build $1 failed:" >&2; tail -20 "$log" >&2; exit 1; }
  grep -oP 'I\s+refs:\s+\K[0-9,]+' "$log" | tr -d ,
}

spill() { # file -> instructions inside the spill pass
  local out="$WORK/$(basename "$1" .vlt).callgrind"
  VELT_THREADS=1 valgrind --tool=callgrind --callgrind-out-file="$out" \
    "$VELT" build "$1" -o "$WORK/$(basename "$1" .vlt)" > /dev/null 2> "$out.log" ||
    { echo "velt build $1 failed:" >&2; tail -20 "$out.log" >&2; exit 1; }
  callgrind_annotate --inclusive=yes "$out" 2> /dev/null |
    awk '!found && /async_fn::spill::spill[^:]/ && !/</ { gsub(",", "", $1); print $1; found = 1 }'
}

if [ -n "$SPILL" ]; then
  printf '%8s %12s %12s %12s %12s\n' N async_M sync_M extra_M spill_M
else
  printf '%8s %12s %12s %12s\n' N async_M sync_M extra_M
fi
extra=0 sync=0
for n in "${SIZES[@]}"; do
  for kind in async sync; do program "$kind" "$n" > "$WORK/${kind}_$n.vlt"; done
  a=$(count "$WORK/async_$n.vlt")
  sync=$(count "$WORK/sync_$n.vlt")
  extra=$((a - sync))
  if [ -n "$SPILL" ]; then
    sp=$(spill "$WORK/async_$n.vlt")
    printf '%8s %12s %12s %12s %12s\n' "$n" $((a / 1000000)) $((sync / 1000000)) $((extra / 1000000)) $((${sp:-0} / 1000000))
  else
    printf '%8s %12s %12s %12s\n' "$n" $((a / 1000000)) $((sync / 1000000)) $((extra / 1000000))
  fi
done
if [ -n "$CHECK" ]; then
  if awk -v e="$extra" -v s="$sync" -v f="$CHECK" 'BEGIN { exit !(e > f * s) }'; then
    echo "FAIL: async extra $extra > $CHECK x sync $sync" >&2
    exit 1
  fi
  echo "ok: async extra is $(awk -v e="$extra" -v s="$sync" 'BEGIN { printf "%.2f", e / s }') x the sync build"
fi
