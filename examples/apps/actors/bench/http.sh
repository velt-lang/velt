#!/usr/bin/env bash
# HTTP comparison: the same wire requests (POST /_sigx/actor/{Type}/{method}, {"args":[key,…]})
# against the Velt runtime and @sigx/actors on Node, loaded with wrk.
#
#   bench/http.sh                     # all servers, all routes
#   SERVERS="velt node" ROUTES="noop" CONNS="64 256" DURATION=10 bench/http.sh
#
# Knobs: VELT (compiler, default ../../../target/release/velt), SIGX_ACTORS (a BUILT
# signalxjs/actors checkout's packages/actors), DURATION (s, 10), WARMUP (s, 3), CONNS
# ("16 64 256"), THREADS (wrk threads, 2), KEYS (distinct actor keys, 1000), VELT_SHARDS (0 = one
# per core), NODE_CPUS / VELT_CPUS / WRK_CPUS (taskset CPU lists; unset = any). SERVERS may also
# name `node-bare`, a plain node:http JSON responder (calibration, no actors).
# Prints one Markdown row per measurement and appends JSON lines to $OUT.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
APP="$(cd "$HERE/.." && pwd)"
VELT="${VELT:-$APP/../../../target/release/velt}"
DURATION="${DURATION:-10}"
WARMUP="${WARMUP:-3}"
CONNS="${CONNS:-16 64 256}"
THREADS="${THREADS:-2}"
KEYS="${KEYS:-1000}"
SERVERS="${SERVERS:-velt node}"
ROUTES="${ROUTES:-noop increment}"
OUT="${OUT:-$HERE/results/http-$(date +%Y%m%d-%H%M%S).jsonl}"
mkdir -p "$(dirname "$OUT")"
WORK="$(mktemp -d)"
PID=""
cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null || true; rm -rf "$WORK"; }
trap cleanup EXIT

(cd "$APP" && "$VELT" build --release -o "$WORK/actors" >/dev/null)

pin() { local cpus="$1"; shift; if [ -n "$cpus" ]; then taskset -c "$cpus" "$@"; else "$@"; fi; }

# Starts a server in the background with $PID its own pid (taskset execs, so no wrapper).
launch() {
  local cpus="$1"; shift
  if [ -n "$cpus" ]; then taskset -c "$cpus" "$@" >"$WORK/server.log" 2>&1 &
  else "$@" >"$WORK/server.log" 2>&1 &
  fi
  PID=$!
}

start() {
  local port
  case "$1" in velt) port=5199 ;; node) port=5299 ;; node-bare) port=5399 ;; esac
  if curl -s -o /dev/null --max-time 1 "http://127.0.0.1:$port/"; then
    echo "port $port is already taken: stop that server first" >&2; exit 1
  fi
  case "$1" in
    velt) launch "${VELT_CPUS:-}" "$WORK/actors" serve --port 5199 --shards "${VELT_SHARDS:-0}"; PORT=5199 ;;
    node) launch "${NODE_CPUS:-}" node --conditions=production "$APP/node/server.mjs" 5299; PORT=5299 ;;
    node-bare) launch "${NODE_CPUS:-}" node "$APP/node/bare.mjs" 5399; PORT=5399 ;;
  esac
  for _ in $(seq 1 100); do
    curl -s -o /dev/null -X POST "http://127.0.0.1:$PORT/_sigx/actor/Tiny/noop" -d '{"args":["up"]}' && return 0
    sleep 0.1
  done
  echo "server $1 did not start:"; cat "$WORK/server.log"; exit 1
}

peak_rss_mb() { awk '/VmHWM/ {printf "%.0f", $2/1024}' "/proc/$PID/status"; }

route_path() { case "$1" in noop) echo /_sigx/actor/Tiny/noop ;; increment) echo /_sigx/actor/Counter/increment ;; esac; }
route_args() { case "$1" in noop) echo "" ;; increment) echo "1" ;; esac; }

echo "| server | route | conns | req/s | p50 | p99 | non-2xx | peak RSS |"
echo "|---|---|---:|---:|---:|---:|---:|---:|"
for s in $SERVERS; do
  start "$s"
  routes="$ROUTES"
  [ "$s" = node-bare ] && routes=noop
  for r in $routes; do
    for c in $CONNS; do
      export ACTOR_PATH="$(route_path "$r")" ARGS="$(route_args "$r")" KEYS
      pin "${WRK_CPUS:-}" wrk -t"$THREADS" -c"$c" -d"${WARMUP}s" -s "$HERE/post.lua" "http://127.0.0.1:$PORT" >/dev/null 2>&1
      pin "${WRK_CPUS:-}" wrk -t"$THREADS" -c"$c" -d"${DURATION}s" --latency -s "$HERE/post.lua" "http://127.0.0.1:$PORT" >"$WORK/wrk.txt" 2>&1
      rps=$(awk '/Requests\/sec/ {print $2}' "$WORK/wrk.txt")
      p50=$(awk '$1=="50%" {print $2}' "$WORK/wrk.txt")
      p99=$(awk '$1=="99%" {print $2}' "$WORK/wrk.txt")
      bad=$(awk '/Non-2xx/ {print $5}' "$WORK/wrk.txt"); bad=${bad:-0}
      rss=$(peak_rss_mb)
      echo "| $s | $r | $c | $rps | $p50 | $p99 | $bad | ${rss} MB |"
      printf '{"server":"%s","route":"%s","conns":%s,"rps":%s,"p50":"%s","p99":"%s","non2xx":%s,"peak_rss_mb":%s}\n' \
        "$s" "$r" "$c" "$rps" "$p50" "$p99" "$bad" "$rss" >>"$OUT"
    done
  done
  kill "$PID"; wait "$PID" 2>/dev/null || true; PID=""
done
