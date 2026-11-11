#!/usr/bin/env bash
# TechEmpower routes by worker count (#2729): Velt on the default scheduler and on the executor in
# Velt (VELT_SCHEDULER=std, a compile-time choice), axum 0.8 and xitca-web, at WORKERS workers
# each (VELT_THREADS, TOKIO_WORKER_THREADS, xitca's worker_threads). The runs are interleaved:
# every round measures each server and route once; the best req/s of RUNS rounds is reported,
# with that run's p99, all runs' req/s and the 1-minute load average before the round.
# Then Velt's instructions per request (cachegrind, one worker; IR=0 skips it).
# Usage: SERVER_CPUS=0,1,6,7 CLIENT_CPUS=2-5 bench/http/techempower/scaling.sh
# Knobs: those of bench/http/lib.sh, plus WORKERS (1 4 8), ROUTES, SERVERS
#        (velt velt-std axum xitca), RUST_TARGET_DIR (cargo target dir for axum and xitca), IR.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/../lib.sh"
WORKERS="${WORKERS:-1 4 8}"
ROUTES="${ROUTES:-plaintext json fortunes}"
SERVERS="${SERVERS:-velt velt-std axum xitca}"
RUST_TARGET_DIR="${RUST_TARGET_DIR:-$WORK/rust-target}"
IR="${IR:-1}"

echo "building..." >&2
"$VELT" build --release "$HERE/server.velt" -o "$WORK/velt" >&2
VELT_SCHEDULER=std "$VELT" build --release "$HERE/server.velt" -o "$WORK/velt-std" >&2
for b in axum xitca; do
  if [[ " $SERVERS " == *" $b "* ]]; then
    (cd "$HERE/$b" && CARGO_TARGET_DIR="$RUST_TARGET_DIR" cargo build --release -q) >&2
  fi
done

# Prints the command line of server $1 with $2 workers on port $PORT.
server_cmd() {
  case "$1" in
    velt) echo "env VELT_THREADS=$2 $WORK/velt $PORT" ;;
    velt-std) echo "env VELT_SCHEDULER=std VELT_THREADS=$2 $WORK/velt-std $PORT" ;;
    axum) echo "env TOKIO_WORKER_THREADS=$2 $RUST_TARGET_DIR/release/techempower-axum $PORT" ;;
    xitca) echo "$RUST_TARGET_DIR/release/techempower-xitca $PORT $2" ;;
  esac
}

declare -A ALL BEST
echo "| workers | server | route | req/s (best of $RUNS) | p99 | all runs req/s | load before rounds |"
echo "|---|---|---|---|---|---|---|"
for w in $WORKERS; do
  loads=""
  for r in $(seq 1 "$RUNS"); do
    loads="$loads $(load1)"
    for s in $SERVERS; do
      PORT="$(random_port)"
      # shellcheck disable=SC2046
      start_server /plaintext $(server_cmd "$s" "$w")
      pid="$STARTED_PID"
      for route in $ROUTES; do
        url="http://127.0.0.1:$PORT/$route"
        warm "$url"
        read -r rps p99 ok < <(measure "$url")
        [ "$ok" = "1.0" ] || echo "warning: $s /$route success rate $ok" >&2
        k="$w $s $route"
        ALL[$k]="${ALL[$k]:-} $rps"
        if [ -z "${BEST[$k]:-}" ] || [ "$rps" -gt "${BEST[$k]%% *}" ]; then BEST[$k]="$rps $p99"; fi
      done
      stop_server "$pid"
    done
  done
  for s in $SERVERS; do
    for route in $ROUTES; do
      k="$w $s $route"
      read -r rps p99 <<<"${BEST[$k]}"
      echo "| $w | $s | /$route | $rps | ${p99} ms |${ALL[$k]} |$loads |"
    done
  done
done

if [ "$IR" = "1" ]; then
  echo
  echo "| server | route | instructions per request (cachegrind, 1 worker) |"
  echo "|---|---|---|"
  for s in $SERVERS; do
    for route in $ROUTES; do
      PORT="$(random_port)"
      # shellcheck disable=SC2046
      ir="$(ir_per_request "/$route" 2000 12000 $(server_cmd "$s" 1))"
      echo "| $s | /$route | $ir |"
    done
  done
fi
