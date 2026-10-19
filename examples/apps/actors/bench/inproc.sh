#!/usr/bin/env bash
# In-process comparison: @sigx/actors' own benchmark scenarios (dispatch/warm-actor,
# dispatch/fan-out-actors, activation/cold-cycle, mem/per-actor-footprint) against
# `actors bench`, the Velt runtime's equivalents (src/bench.vlt), on this machine.
#
#   bench/inproc.sh          # needs SIGX_ACTORS_REPO: a BUILT signalxjs/actors checkout
#
# Velt runs twice: on every core (one shard per core; callers on any core) and pinned to one
# core with one shard (the shape of Node's single thread). Output: results/inproc-*.txt
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
APP="$(cd "$HERE/.." && pwd)"
VELT="${VELT:-$APP/../../../target/release/velt}"
REPO="${SIGX_ACTORS_REPO:-$APP/../../../../actors}"
DURATION="${DURATION:-1000}"
OUT="${OUT:-$HERE/results}"
mkdir -p "$OUT"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
(cd "$APP" && "$VELT" build --release -o "$WORK/actors" >/dev/null)

echo "== velt, all cores"
"$WORK/actors" bench --duration "$DURATION" | tee "$OUT/inproc-velt-all-cores.txt"
echo "== velt, one core"
taskset -c 0 "$WORK/actors" bench --duration "$DURATION" --shards 1 | tee "$OUT/inproc-velt-one-core.txt"
echo "== node (@sigx/actors benchmarks)"
(cd "$REPO" && node --conditions=production --expose-gc benchmarks/src/main.ts \
  dispatch/warm-actor dispatch/fan-out-actors activation/cold-cycle mem/per-actor-footprint \
  --runs=3 --json="$OUT/inproc-node.json") 2>&1 | tee "$OUT/inproc-node.txt"
