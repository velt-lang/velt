#!/usr/bin/env bash
# The middleware rows of #2726 step 0: server.velt with 0, 1 and 3 middleware of the `next` shape
# capturing `req`. Per count: instructions per request (cachegrind, one worker), req/s and p99 at
# MW_WORKERS workers (best of RUNS interleaved rounds) and the peak RSS after those runs.
# Then (SOAK=1, the default) the soak: SOAK_REQUESTS requests to the server with SOAK_MW
# middleware (3), the RSS printed every SOAK_STEP requests; fails (exit 1) if the RSS keeps
# growing: the last reading above the highest one of the first half by more than
# SOAK_MAX_BYTES (256) per request of the second half. The allocator's heaps grow in steps of a
# few MiB for a while; a leak like #2719's (about 40 KB per request and hop) is 100x the bound.
# Usage: SERVER_CPUS=0,1,6,7 CLIENT_CPUS=2-5 bench/http/middleware/run.sh
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/../lib.sh"
COUNTS="${COUNTS:-0 1 3}"
MW_WORKERS="${MW_WORKERS:-4}"
SOAK="${SOAK:-1}"
SOAK_REQUESTS="${SOAK_REQUESTS:-100000}"
SOAK_STEP="${SOAK_STEP:-10000}"
SOAK_MAX_BYTES="${SOAK_MAX_BYTES:-256}"
SOAK_MW="${SOAK_MW:-3}"

"$VELT" build --release "$HERE/server.velt" -o "$WORK/mw" >&2

if [ "${ROWS:-1}" = "1" ]; then
  declare -A ALL BEST HWM
  loads=""
  for r in $(seq 1 "$RUNS"); do
    loads="$loads $(load1)"
    for n in $COUNTS; do
      PORT="$(random_port)"
      start_server / env VELT_THREADS="$MW_WORKERS" "$WORK/mw" "$PORT" "$n"
      pid="$STARTED_PID"
      url="http://127.0.0.1:$PORT/"
      warm "$url"
      read -r rps p99 ok < <(measure "$url")
      [ "$ok" = "1.0" ] || echo "warning: $n middleware success rate $ok" >&2
      read -r hwm _ < <(rss_kib "$pid")
      ALL[$n]="${ALL[$n]:-} $rps"
      if [ -z "${BEST[$n]:-}" ] || [ "$rps" -gt "${BEST[$n]%% *}" ]; then BEST[$n]="$rps $p99"; fi
      if [ -z "${HWM[$n]:-}" ] || [ "$hwm" -gt "${HWM[$n]}" ]; then HWM[$n]="$hwm"; fi
      stop_server "$pid"
    done
  done
  echo "| middleware | instructions per request | req/s ($MW_WORKERS workers, best of $RUNS) | p99 | all runs | peak RSS | load before rounds |"
  echo "|---|---|---|---|---|---|---|"
  for n in $COUNTS; do
    PORT="$(random_port)"
    ir="$(ir_per_request / 2000 12000 env VELT_THREADS=1 "$WORK/mw" "$PORT" "$n")"
    read -r rps p99 <<<"${BEST[$n]}"
    echo "| $n | $ir | $rps | ${p99} ms |${ALL[$n]} | $((HWM[$n] / 1024)) MiB |$loads |"
  done
fi

if [ "$SOAK" = "1" ]; then
  PORT="$(random_port)"
  start_server / env VELT_THREADS="$MW_WORKERS" "$WORK/mw" "$PORT" "$SOAK_MW"
  pid="$STARTED_PID"
  echo
  echo "soak: $SOAK_REQUESTS requests to $SOAK_MW middleware, $MW_WORKERS workers, 64 connections (load $(load1))"
  echo "| requests | RSS | peak RSS |"
  echo "|---|---|---|"
  readings=()
  for done_ in $(seq "$SOAK_STEP" "$SOAK_STEP" "$SOAK_REQUESTS"); do
    fire "http://127.0.0.1:$PORT/" "$SOAK_STEP" 64
    read -r hwm rss < <(rss_kib "$pid")
    readings+=("$rss")
    echo "| $done_ | $((rss / 1024)) MiB ($rss KiB) | $((hwm / 1024)) MiB |"
  done
  stop_server "$pid"
  first=0
  for x in "${readings[@]:0:$(((${#readings[@]} + 1) / 2))}"; do
    if [ "$x" -gt "$first" ]; then first="$x"; fi
  done
  last="${readings[-1]}"
  second=$((SOAK_REQUESTS - SOAK_STEP * ((${#readings[@]} + 1) / 2)))
  per=$(((last - first) * 1024 / (second > 0 ? second : 1)))
  echo "first half's peak $first KiB, $last KiB at the end: $per bytes per request of the second half"
  if [ "$per" -gt "$SOAK_MAX_BYTES" ]; then
    echo "soak FAILED: the RSS keeps growing (over $SOAK_MAX_BYTES bytes per request)" >&2
    exit 1
  fi
  echo "soak flat (at most $SOAK_MAX_BYTES bytes per request)"
fi
