#!/usr/bin/env bash
# TechEmpower-style load test: Velt vs Node (single process and node:cluster) vs axum.
# Usage: bench/http/techempower/run.sh   (from anywhere; needs oha, node, cargo)
# Knobs: DURATION (10s) CONNS (256) RUNS (2: best req/s is reported) ROUTES SERVERS
#        AXUM_TARGET_DIR (cargo target dir for the axum baseline; default: a temp dir).
# Prints one Markdown table row per server/route. Only kills the servers it started.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
DURATION="${DURATION:-10s}"
CONNS="${CONNS:-256}"
RUNS="${RUNS:-2}"
ROUTES="${ROUTES:-plaintext json fortunes}"
SERVERS="${SERVERS:-velt node node-cluster axum}"
WORK="$(mktemp -d)"
AXUM_TARGET_DIR="${AXUM_TARGET_DIR:-$WORK/axum-target}"
PIDS=()

cleanup() {
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
  rm -rf "$WORK"
}
trap cleanup EXIT

random_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}

build() {
  echo "building velt server (release)..." >&2
  "$ROOT/target/debug/velt" build --release "$HERE/server.vlt" -o "$WORK/velt-server" >&2
  if [[ " $SERVERS " == *" axum "* ]]; then
    echo "building axum baseline (release)..." >&2
    (cd "$HERE/axum" && CARGO_TARGET_DIR="$AXUM_TARGET_DIR" cargo build --release -q) >&2
  fi
}

# Starts server $1 on port $2 in the background; sets STARTED_PID.
start() {
  case "$1" in
    velt) "$WORK/velt-server" "$2" >/dev/null 2>&1 & ;;
    node) node "$HERE/server.mjs" "$2" >/dev/null 2>&1 & ;;
    node-cluster) CLUSTER=1 node "$HERE/server.mjs" "$2" >/dev/null 2>&1 & ;;
    axum) "$AXUM_TARGET_DIR/release/techempower-axum" "$2" >/dev/null 2>&1 & ;;
  esac
  STARTED_PID=$!
  PIDS+=("$STARTED_PID")
  for _ in $(seq 1 100); do
    curl -sf "http://127.0.0.1:$2/plaintext" >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  echo "server $1 did not start" >&2
  return 1
}

# oha JSON summary -> "req/s avg_ms p99_ms success_rate".
measure() {
  oha -z "$DURATION" -c "$CONNS" --no-tui --output-format json "$1" |
    python3 -c 'import json, sys
d = json.load(sys.stdin)
s, p = d["summary"], d["latencyPercentiles"]
print("%.0f %.2f %.2f %s" % (s["requestsPerSec"], s["average"] * 1000, p["p99"] * 1000, s["successRate"]))'
}

build
echo "| server | route | req/s (best of $RUNS) | avg latency | p99 | all runs req/s |"
echo "|---|---|---|---|---|---|"
for server in $SERVERS; do
  port="$(random_port)"
  start "$server" "$port"
  for route in $ROUTES; do
    url="http://127.0.0.1:$port/$route"
    oha -z 2s -c "$CONNS" --no-tui --output-format quiet "$url" >/dev/null 2>&1 || true
    best=""
    all=""
    for _ in $(seq 1 "$RUNS"); do
      read -r rps avg p99 ok < <(measure "$url")
      [ "$ok" = "1.0" ] || echo "warning: $server /$route success rate $ok" >&2
      all="$all ${rps}"
      if [ -z "$best" ] || [ "$rps" -gt "${best%% *}" ]; then
        best="$rps $avg $p99 $ok"
      fi
    done
    read -r rps avg p99 ok <<<"$best"
    echo "| $server | /$route | $rps | ${avg} ms | ${p99} ms |${all} |"
  done
  kill "$STARTED_PID" 2>/dev/null || true
  wait "$STARTED_PID" 2>/dev/null || true
done
