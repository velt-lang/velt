#!/usr/bin/env bash
# CPU time per call: the comparison that survives a busy machine, where throughput does not.
#
#   bench/cpu.sh inproc    # `actors calls` against node/calls.mjs (sigx's host.dispatch)
#   bench/cpu.sh http      # the wire endpoint under oha: server CPU per request, peak memory
#   bench/cpu.sh ir        # instructions per call in process (cachegrind; Linux only)
#
# In process, each configuration runs twice, with N1 and N2 calls, and the result is
# Δ process CPU / Δ calls, which cancels startup and warmup. Over HTTP, the server's CPU time is
# read before and after a run of N requests (after a warmup of N/5), and its peak memory after.
# Every figure is printed for all CPU time (user + system, all threads) and for user mode only.
# `ir` counts instructions the same way, Δ between N1 and N2 calls (valgrind runs the threads
# one at a time, so the count does not depend on load).
#
# Linux, and Windows with Git Bash and pwsh. Not macOS: it reads /proc and uses GNU time.
# Needs node 22.18+, oha for `http`, valgrind for `ir`, and SIGX_ACTORS_REPO: a BUILT
# signalxjs/actors checkout. Knobs: VELT, N1, N2, N, REPS (runs of everything, default 2),
# CONNS, SCENARIOS, SERVERS. Output: one line per measurement; the README quotes the lower of
# the REPS runs.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
APP="$(cd "$HERE/.." && pwd)"
VELT="${VELT:-$APP/../../../target/release/velt}"
REPO="${SIGX_ACTORS_REPO:-$APP/../../../../actors}"
export SIGX_ACTORS_REPO="$REPO" SIGX_ACTORS="${SIGX_ACTORS:-$REPO/packages/actors}"
REPS="${REPS:-2}"
WORK="$(mktemp -d)"
PID=""
cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null || true; rm -rf "$WORK"; }
trap cleanup EXIT
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) WINDOWS=1 ;;
  Linux) WINDOWS=0 ;;
  *) echo "cpu.sh runs on Linux and on Windows (Git Bash), not on $(uname -s)" >&2; exit 2 ;;
esac
EXE="$WORK/actors"
[ "$WINDOWS" = 1 ] && EXE="$EXE.exe"
(cd "$APP" && "$VELT" build --release -o "$EXE" >/dev/null)

# "<cpu ms> <user ms> <wall ms>" of a command run to completion (cpu = user + system).
cpu_of() {
  if [ "$WINDOWS" = 1 ]; then
    pwsh -NoProfile -File "$(cygpath -w "$HERE/cpu.ps1")" "$@" | tr -d '\r'
  else
    /usr/bin/time -f "%U %S %e" "$@" 2>&1 >/dev/null | tail -1 |
      awk '{printf "%d %d %d", ($1 + $2) * 1000, $1 * 1000, $3 * 1000}'
  fi
}

# "<cpu ms> <user ms> <peak MB>" of the running process $PID so far (peak working set on
# Windows, VmHWM on Linux).
cpu_now() {
  if [ "$WINDOWS" = 1 ]; then
    pwsh -NoProfile -Command "\$p = Get-Process -Id $(cat /proc/$PID/winpid); '{0:F0} {1:F0} {2:F0}' -f \$p.TotalProcessorTime.TotalMilliseconds, \$p.UserProcessorTime.TotalMilliseconds, (\$p.PeakWorkingSet64 / 1MB)" | tr -d '\r'
  else
    local hz
    hz="$(getconf CLK_TCK)"
    awk -v hz="$hz" '{printf "%d %d ", ($14 + $15) * 1000 / hz, $14 * 1000 / hz}' "/proc/$PID/stat"
    awk '/^VmHWM:/ {printf "%d", $2 / 1024}' "/proc/$PID/status"
  fi
}

# One `calls` run of configuration $1 (velt4 | velt1 | node): scenario $2, $3 callers, $4 calls.
calls() {
  case $1 in
    velt4) cpu_of "$EXE" calls --scenario "$2" --c "$3" --n "$4" --shards 4 ;;
    velt1) VELT_THREADS=1 cpu_of "$EXE" calls --scenario "$2" --c "$3" --n "$4" --shards 1 ;;
    node) cpu_of node --conditions=production "$APP/node/calls.mjs" "$2" "$3" "$4" ;;
  esac
}

inproc() {
  local n1="${N1:-20000}" n2="${N2:-220000}"
  for _ in $(seq 1 "$REPS"); do
    for sc in ${SCENARIOS:-warm fan}; do
      for c in ${CONNS:-1 64 512}; do
        for cfg in velt4 velt1 node; do
          local a b
          a=$(calls "$cfg" "$sc" "$c" "$n1")
          b=$(calls "$cfg" "$sc" "$c" "$n2")
          awk -v cfg="$cfg" -v sc="$sc" -v c="$c" -v a="$a" -v b="$b" -v n=$((n2 - n1)) 'BEGIN {
            split(a, x, " "); split(b, y, " ")
            printf "%s %s c=%s cpu_us/call=%.2f user_us/call=%.2f wall_ms(N2)=%d\n",
              cfg, sc, c, (y[1] - x[1]) * 1000 / n, (y[2] - x[2]) * 1000 / n, y[3]
          }'
        done
      done
    done
  done
}

http() {
  local n="${N:-40000}"
  for i in $(seq 0 999); do echo "{\"args\":[\"k$i\"]}"; done >"$WORK/noop.lines"
  for i in $(seq 0 999); do echo "{\"args\":[\"k$i\",1]}"; done >"$WORK/incr.lines"
  for _ in $(seq 1 "$REPS"); do
    for s in ${SERVERS:-velt velt1 node bare}; do
      case $s in velt | velt1) PORT=5199 ;; node) PORT=5299 ;; bare) PORT=5399 ;; esac
      if curl -s -o /dev/null --max-time 1 "http://127.0.0.1:$PORT/"; then
        echo "port $PORT is already taken: stop that server first" >&2
        exit 1
      fi
      case $s in
        velt) "$EXE" serve --port $PORT >"$WORK/server.log" 2>&1 & ;;
        velt1) "$EXE" serve --port $PORT --shards 1 >"$WORK/server.log" 2>&1 & ;;
        node) node --conditions=production "$APP/node/server.mjs" $PORT >"$WORK/server.log" 2>&1 & ;;
        bare) node "$APP/node/bare.mjs" $PORT >"$WORK/server.log" 2>&1 & ;;
      esac
      PID=$!
      for _ in $(seq 1 100); do
        curl -s -o /dev/null -X POST "http://127.0.0.1:$PORT/_sigx/actor/Tiny/noop" -d '{"args":["up"]}' && break
        sleep 0.2
      done
      for r in noop incr; do
        [ "$s" = bare ] && [ "$r" = incr ] && continue
        local path=Tiny/noop conns="${CONNS:-16 64 256}"
        if [ "$r" = incr ]; then path=Counter/increment conns="${INCR_CONNS:-64}"; fi
        for c in $conns; do
          local url="http://127.0.0.1:$PORT/_sigx/actor/$path" a b
          oha --no-tui -n $((n / 5)) -c "$c" -m POST -H 'content-type: application/json' -Z "$WORK/$r.lines" "$url" >/dev/null 2>&1
          a=$(cpu_now)
          oha --no-tui -n "$n" -c "$c" -m POST -H 'content-type: application/json' -Z "$WORK/$r.lines" "$url" >"$WORK/oha.txt" 2>&1
          b=$(cpu_now)
          awk -v s="$s" -v r="$r" -v c="$c" -v n="$n" -v a="$a" -v b="$b" '
            /Requests\/sec/ { rps = $2 }
            /^ *50\.00% in/ && p50 == "" { p50 = $3 " " $4 }
            /^ *99\.00% in/ && p99 == "" { p99 = $3 " " $4 }
            /\[200\]/ { ok = $2 }
            END {
              split(a, x, " "); split(b, y, " ")
              printf "%s %s c=%s n=%s ok=%s cpu_us/req=%.1f user_us/req=%.1f rps=%s p50=%s p99=%s peakMB=%s\n",
                s, r, c, n, ok, (y[1] - x[1]) * 1000 / n, (y[2] - x[2]) * 1000 / n, rps, p50, p99, y[3]
            }' "$WORK/oha.txt"
        done
      done
      if [ "$WINDOWS" = 1 ]; then
        taskkill //F //PID "$(cat "/proc/$PID/winpid")" >/dev/null 2>&1 || true
      fi
      kill "$PID" 2>/dev/null || true
      wait "$PID" 2>/dev/null || true
      PID=""
    done
  done
}

# Node is not counted: V8 does not survive valgrind.
ir() {
  [ "$WINDOWS" = 0 ] || { echo "cpu.sh ir needs Linux (valgrind)" >&2; exit 2; }
  local n1="${N1:-10000}" n2="${N2:-60000}"
  for cfg in velt1 velt4; do
    for sc in ${SCENARIOS:-warm fan}; do
      for c in ${CONNS:-1 64}; do
        for n in "$n1" "$n2"; do
          local threads=1 shards=1
          [ "$cfg" = velt4 ] && threads=4 shards=4
          VELT_THREADS=$threads valgrind --vgdb=no --tool=cachegrind --cache-sim=no \
            --cachegrind-out-file=/dev/null "$EXE" calls --scenario "$sc" --c "$c" --n "$n" \
            --shards $shards >/dev/null 2>"$WORK/vg.log"
          echo "$cfg $sc $c $n $(sed -n 's/.*I *refs: *\([0-9,]*\).*/\1/p' "$WORK/vg.log" | tr -d ,)"
        done
      done
    done
  done
}

case "${1:-}" in
  inproc) inproc ;;
  http) http ;;
  ir) ir | sort ;;
  *) echo "usage: bench/cpu.sh inproc | http | ir" >&2; exit 2 ;;
esac
