#!/usr/bin/env bash
# CPU time per call: the comparison that survives a busy machine, where throughput does not.
#
#   bench/cpu.sh inproc    # `actors calls` against node/calls.mjs (sigx's host.dispatch)
#   bench/cpu.sh http      # the wire endpoint under oha, server CPU per request
#
# In process, each configuration runs twice, with N1 and N2 calls, and the result is
# Δ process CPU / Δ calls, which cancels startup and warmup. Over HTTP, the server's CPU time is
# read before and after a run of N requests (after a warmup of N/5). Linux and Windows (Git Bash,
# with pwsh); needs node 22.18+, oha for `http`, and SIGX_ACTORS_REPO: a BUILT signalxjs/actors
# checkout. Knobs: VELT, N1, N2, N, CONNS, SCENARIOS. Output: one line per measurement.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
APP="$(cd "$HERE/.." && pwd)"
VELT="${VELT:-$APP/../../../target/release/velt}"
REPO="${SIGX_ACTORS_REPO:-$APP/../../../../actors}"
export SIGX_ACTORS_REPO="$REPO" SIGX_ACTORS="${SIGX_ACTORS:-$REPO/packages/actors}"
WORK="$(mktemp -d)"
PID=""
cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null || true; rm -rf "$WORK"; }
trap cleanup EXIT
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) WINDOWS=1 ;; *) WINDOWS=0 ;; esac
EXE="$WORK/actors"
[ "$WINDOWS" = 1 ] && EXE="$EXE.exe"
(cd "$APP" && "$VELT" build --release -o "$EXE" >/dev/null)

# CPU milliseconds (user + system) of a command run to completion.
cpu_of() {
  if [ "$WINDOWS" = 1 ]; then
    pwsh -NoProfile -File "$(cygpath -w "$HERE/cpu.ps1")" "$@" | tr -d '\r'
  else
    /usr/bin/time -f "%U %S" "$@" 2>&1 >/dev/null | tail -1 | awk '{printf "%d", ($1 + $2) * 1000}'
  fi
}

# CPU milliseconds used so far by the running process $PID.
cpu_now() {
  if [ "$WINDOWS" = 1 ]; then
    pwsh -NoProfile -Command "[int](Get-Process -Id $(cat /proc/$PID/winpid)).TotalProcessorTime.TotalMilliseconds" | tr -d '\r'
  else
    awk -v hz="$(getconf CLK_TCK)" '{printf "%d", ($14 + $15) * 1000 / hz}' "/proc/$PID/stat"
  fi
}

inproc() {
  local n1="${N1:-20000}" n2="${N2:-220000}"
  for sc in ${SCENARIOS:-warm fan}; do
    for c in ${CONNS:-1 64 512}; do
      for cfg in velt-4-shards velt-1-thread node; do
        local a b
        case $cfg in
          velt-4-shards)
            a=$(cpu_of "$EXE" calls --scenario "$sc" --c "$c" --n "$n1" --shards 4)
            b=$(cpu_of "$EXE" calls --scenario "$sc" --c "$c" --n "$n2" --shards 4) ;;
          velt-1-thread)
            a=$(VELT_THREADS=1 cpu_of "$EXE" calls --scenario "$sc" --c "$c" --n "$n1" --shards 1)
            b=$(VELT_THREADS=1 cpu_of "$EXE" calls --scenario "$sc" --c "$c" --n "$n2" --shards 1) ;;
          node)
            a=$(cpu_of node --conditions=production "$APP/node/calls.mjs" "$sc" "$c" "$n1")
            b=$(cpu_of node --conditions=production "$APP/node/calls.mjs" "$sc" "$c" "$n2") ;;
        esac
        awk -v cfg="$cfg" -v sc="$sc" -v c="$c" -v a="$a" -v b="$b" -v n=$((n2 - n1)) \
          'BEGIN { printf "%s %s c=%s cpu_us/call=%.2f\n", cfg, sc, c, (b - a) * 1000 / n }'
      done
    done
  done
}

http() {
  local n="${N:-40000}"
  for i in $(seq 0 999); do echo "{\"args\":[\"k$i\"]}"; done >"$WORK/noop.lines"
  for i in $(seq 0 999); do echo "{\"args\":[\"k$i\",1]}"; done >"$WORK/increment.lines"
  for s in ${SERVERS:-velt velt-1-shard node node-bare}; do
    case $s in
      velt) "$EXE" serve --port 5199 >"$WORK/server.log" 2>&1 & PORT=5199 ;;
      velt-1-shard) "$EXE" serve --port 5199 --shards 1 >"$WORK/server.log" 2>&1 & PORT=5199 ;;
      node) node --conditions=production "$APP/node/server.mjs" 5299 >"$WORK/server.log" 2>&1 & PORT=5299 ;;
      node-bare) node "$APP/node/bare.mjs" 5399 >"$WORK/server.log" 2>&1 & PORT=5399 ;;
    esac
    PID=$!
    for _ in $(seq 1 100); do
      curl -s -o /dev/null -X POST "http://127.0.0.1:$PORT/_sigx/actor/Tiny/noop" -d '{"args":["up"]}' && break
      sleep 0.2
    done
    for r in noop increment; do
      [ "$s" = node-bare ] && [ "$r" = increment ] && continue
      local path=Tiny/noop
      [ "$r" = increment ] && path=Counter/increment
      for c in ${CONNS:-16 64 256}; do
        local url="http://127.0.0.1:$PORT/_sigx/actor/$path"
        oha --no-tui -n $((n / 5)) -c "$c" -m POST -H 'content-type: application/json' -Z "$WORK/$r.lines" "$url" >/dev/null 2>&1
        local a b
        a=$(cpu_now)
        oha --no-tui -n "$n" -c "$c" -m POST -H 'content-type: application/json' -Z "$WORK/$r.lines" "$url" >"$WORK/oha.txt" 2>&1
        b=$(cpu_now)
        awk -v s="$s" -v r="$r" -v c="$c" -v a="$a" -v b="$b" -v n="$n" \
          -v rps="$(awk '/Requests\/sec/ {print $2; exit}' "$WORK/oha.txt")" \
          'BEGIN { printf "%s %s c=%s cpu_us/req=%.1f req/s=%.0f\n", s, r, c, (b - a) * 1000 / n, rps }'
      done
    done
    if [ "$WINDOWS" = 1 ]; then
      taskkill //F //PID "$(cat "/proc/$PID/winpid")" >/dev/null 2>&1 || true
    fi
    kill "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
    PID=""
  done
}

case "${1:-}" in
  inproc) inproc ;;
  http) http ;;
  *) echo "usage: bench/cpu.sh inproc | http" >&2; exit 2 ;;
esac
