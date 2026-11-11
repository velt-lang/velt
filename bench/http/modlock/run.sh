#!/usr/bin/env bash
# The module-lock row and the skewed-load row of #2729, on server.velt.
# Module lock: mode "module" (the handler reads a module-level Map) vs "param" (the same map made
# in main and captured by the handler), at WORKERS workers, on the default scheduler and on the
# executor in Velt (VELT_SCHEDULER=std). Best req/s of RUNS interleaved rounds, that run's p99,
# and instructions per request (cachegrind, one worker).
# Skewed load (SKEW=1): SKEW_LIGHT keep-alive connections on /light and SKEW_HEAVY on /heavy (10x
# the work) at the same time, at SKEW_WORKERS workers; req/s and p99 of each class and the busy
# share of each server thread over the run (from /proc/<pid>/task/*/stat, threads under 2% left
# out).
# Usage: SERVER_CPUS=0,1,6,7 CLIENT_CPUS=2-5 bench/http/modlock/run.sh
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/../lib.sh"
WORKERS="${WORKERS:-1 4 8}"
SCHEDS="${SCHEDS:-velt velt-std}"
MODES="${MODES:-module param}"
SKEW="${SKEW:-1}"
SKEW_WORKERS="${SKEW_WORKERS:-4 8}"
SKEW_LIGHT="${SKEW_LIGHT:-6}"
SKEW_HEAVY="${SKEW_HEAVY:-2}"

"$VELT" build --release "$HERE/server.velt" -o "$WORK/velt" >&2
VELT_SCHEDULER=std "$VELT" build --release "$HERE/server.velt" -o "$WORK/velt-std" >&2

# Command line for scheduler $1, $2 workers, mode $3 (port $PORT).
cmd() {
  local extra=""
  [ "$1" = "velt-std" ] && extra="VELT_SCHEDULER=std"
  echo "env $extra VELT_THREADS=$2 $WORK/$1 $PORT $3"
}

if [ "${ROWS:-1}" = "1" ]; then
  declare -A ALL BEST
  echo "| workers | scheduler | mode | req/s (best of $RUNS) | p99 | all runs | load before rounds |"
  echo "|---|---|---|---|---|---|---|"
  for w in $WORKERS; do
    loads=""
    for r in $(seq 1 "$RUNS"); do
      loads="$loads $(load1)"
      for s in $SCHEDS; do
        for m in $MODES; do
          PORT="$(random_port)"
          # shellcheck disable=SC2046
          start_server /light $(cmd "$s" "$w" "$m")
          pid="$STARTED_PID"
          url="http://127.0.0.1:$PORT/light"
          warm "$url"
          read -r rps p99 ok < <(measure "$url")
          [ "$ok" = "1.0" ] || echo "warning: $s $m success rate $ok" >&2
          k="$w $s $m"
          ALL[$k]="${ALL[$k]:-} $rps"
          if [ -z "${BEST[$k]:-}" ] || [ "$rps" -gt "${BEST[$k]%% *}" ]; then BEST[$k]="$rps $p99"; fi
          stop_server "$pid"
        done
      done
    done
    for s in $SCHEDS; do
      for m in $MODES; do
        k="$w $s $m"
        read -r rps p99 <<<"${BEST[$k]}"
        echo "| $w | $s | $m | $rps | ${p99} ms |${ALL[$k]} |$loads |"
      done
    done
  done
  echo
  echo "| scheduler | mode | instructions per request (cachegrind, 1 worker) |"
  echo "|---|---|---|"
  for s in $SCHEDS; do
    for m in $MODES; do
      PORT="$(random_port)"
      # shellcheck disable=SC2046
      echo "| $s | $m | $(ir_per_request /light 2000 12000 $(cmd "$s" 1 "$m")) |"
    done
  done
fi

if [ "$SKEW" = "1" ]; then
  secs="${DURATION%s}"
  echo
  echo "| workers | scheduler | /light req/s | /light p99 | /heavy req/s | /heavy p99 | thread busy share | load |"
  echo "|---|---|---|---|---|---|---|---|"
  for w in $SKEW_WORKERS; do
    for s in $SCHEDS; do
      l="$(load1)"
      PORT="$(random_port)"
      # shellcheck disable=SC2046
      start_server /light $(cmd "$s" "$w" param)
      pid="$STARTED_PID"
      warm "http://127.0.0.1:$PORT/light" 8
      thread_ticks "$pid" >"$WORK/t0"
      measure "http://127.0.0.1:$PORT/heavy" "$SKEW_HEAVY" >"$WORK/heavy" &
      hpid=$!
      measure "http://127.0.0.1:$PORT/light" "$SKEW_LIGHT" >"$WORK/light"
      wait "$hpid"
      thread_ticks "$pid" >"$WORK/t1"
      busy="$(thread_busy "$WORK/t0" "$WORK/t1" "$secs")"
      read -r lr lp _ <"$WORK/light"
      read -r hr hp _ <"$WORK/heavy"
      echo "| $w | $s | $lr | $lp ms | $hr | $hp ms | $busy | $l |"
      stop_server "$pid"
    done
  done
fi
