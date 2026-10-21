#!/usr/bin/env bash
# Wall time of one benchmark built by several compilers (Linux): ROUNDS rounds (default 5),
# each running every build 5 times in turn, interleaved so that a change in the machine's load
# hits all builds alike. Prints each build's best time per round and the median of those bests.
# For a program whose wall time matters but whose instruction count does not tell (threads that
# wait for each other, cache misses), such as bench/async/timers_fire.vlt.
#
#   ROUNDS=5 bench/async/wall_rounds.sh PROGRAM.vlt NAME=VELT [NAME=VELT ...]
#
# Each VELT is a `velt` binary (with its release runtime next to it); the program is built with
# `velt build --release` into $TMPDIR.
set -euo pipefail
if [ $# -lt 2 ]; then
  echo "usage: bench/async/wall_rounds.sh PROGRAM.vlt NAME=VELT [NAME=VELT ...]" >&2
  exit 2
fi
PROGRAM=$1
shift
ROUNDS=${ROUNDS:-5}
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT
names=()
for arg in "$@"; do
  name=${arg%%=*}
  "${arg#*=}" build --release "$PROGRAM" -o "$OUT/$name" >/dev/null
  names+=("$name")
done
now_ms() { date +%s%N | awk '{ printf "%d", $1 / 1000000 }'; }
declare -A bests
for r in $(seq 1 "$ROUNDS"); do
  declare -A best=()
  for i in 1 2 3 4 5; do
    for name in "${names[@]}"; do
      s=$(now_ms)
      "$OUT/$name" >/dev/null
      ms=$(($(now_ms) - s))
      if [ -z "${best[$name]:-}" ] || [ "$ms" -lt "${best[$name]}" ]; then best[$name]=$ms; fi
    done
  done
  line="round $r:"
  for name in "${names[@]}"; do
    line="$line $name ${best[$name]} ms"
    bests[$name]="${bests[$name]:-} ${best[$name]}"
  done
  echo "$line"
done
for name in "${names[@]}"; do
  median=$(echo "${bests[$name]}" | tr ' ' '\n' | sed '/^$/d' | sort -n | sed -n "$(((ROUNDS + 1) / 2))p")
  echo "$name: bests${bests[$name]} ms; median $median ms"
done
