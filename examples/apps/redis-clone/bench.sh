#!/usr/bin/env bash
# This server vs a real redis-server with redis-benchmark. Runs parity.vlt first (byte-identical
# replies), then each test against each server in turn, three times, and prints the median
# requests/s. The Velt server runs twice: on one worker thread and on all of them (the default).
# Last, memory: RSS after the same million SETs.
#   ./bench.sh [requests per run] [connections]     (default 200000, 50; needs redis-server)
set -euo pipefail
cd "$(dirname "$0")"
n=${1:-200000}
conns=${2:-50}
# A release velt: a debug one links the debug runtime, which is several times slower.
velt=${VELT:-../../../target/release/velt}
[ -x "$velt" ] || velt=velt
rport=${REDIS_PORT:-17379}
v1port=${VELT1_PORT:-17380}
vnport=${VELTN_PORT:-17381}
tmp=$(mktemp -d)
pids=()
cleanup() {
    for p in ${pids[@]+"${pids[@]}"}; do kill "$p" 2>/dev/null || true; done
    rm -rf "$tmp"
}
trap cleanup EXIT

"$velt" build --release -o "$tmp/server" >/dev/null
start_all() {
    redis-server --port "$rport" --save "" --appendonly no >/dev/null &
    pids+=($!)
    VELT_THREADS=1 "$tmp/server" "$v1port" >/dev/null &
    pids+=($!)
    "$tmp/server" "$vnport" >/dev/null &
    pids+=($!)
    for port in $rport $v1port $vnport; do
        for _ in $(seq 50); do
            redis-cli -p "$port" ping >/dev/null 2>&1 && break
            sleep 0.1
        done
    done
}
stop_all() {
    for p in ${pids[@]+"${pids[@]}"}; do kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    pids=()
}

start_all
"$velt" run parity.vlt -- "127.0.0.1:$v1port" "127.0.0.1:$rport"

rps() { # rps <port> <test> [extra args]: requests per second of one redis-benchmark run
    local port=$1 test=$2
    shift 2
    redis-benchmark -p "$port" -q -n "$n" -c "$conns" -t "$test" "$@" 2>/dev/null |
        tr '\r' '\n' | grep 'requests per second' | tail -1 | sed 's/.*: \([0-9.]*\) requests per second.*/\1/' | awk '{print int($1)}'
}
median() { printf '%s\n' "$@" | sort -n | sed -n 2p; }

row() { # row <label> <test> [extra args]
    local label=$1 test=$2
    shift 2
    local r=() a=() b=()
    for _ in 1 2 3; do
        r+=("$(rps "$rport" "$test" "$@")")
        a+=("$(rps "$v1port" "$test" "$@")")
        b+=("$(rps "$vnport" "$test" "$@")")
    done
    printf '%-22s %12s %14s %14s\n' "$label" "$(median "${r[@]}")" "$(median "${a[@]}")" "$(median "${b[@]}")"
}

echo
printf '%-22s %12s %14s %14s\n' "requests/s (median of 3)" "redis" "velt 1 thread" "velt default"
for t in ping_mbulk set get incr lpush lpop sadd hset zadd lrange_100 mset; do
    row "$t" "$t"
done
row "set, pipeline 16" set -P 16
row "get, pipeline 16" get -P 16

# CPU time each server spends on the same million GETs (steadier than requests/s on a busy
# machine): ps reports the process's CPU time as [h:]m:ss.cc.
cpu_ms() { ps -o time= -p "$1" | awk -F'[:.]' '{ if (NF == 3) print ($1 * 60 + $2) * 1000 + $3 * 10; else print (($1 * 60 + $2) * 60 + $3) * 1000 + $4 * 10 }'; }
echo
for i in 0 1 2; do
    port=$([ $i = 0 ] && echo $rport || ([ $i = 1 ] && echo $v1port || echo $vnport))
    label=$([ $i = 0 ] && echo "redis" || ([ $i = 1 ] && echo "velt 1 thread" || echo "velt default"))
    before=$(cpu_ms "${pids[$i]}")
    redis-benchmark -p "$port" -q -n 1000000 -c "$conns" -t get >/dev/null 2>&1
    after=$(cpu_ms "${pids[$i]}")
    printf '%-14s %6s ms CPU per million GETs (%s us per request)\n' "$label" $((after - before)) \
        "$(awk -v ms=$((after - before)) 'BEGIN { printf "%.1f", ms / 1000 }')"
done

# Memory: the same million keys (16-byte values) in each.
for port in $rport $v1port $vnport; do
    redis-cli -p "$port" flushall >/dev/null
    redis-benchmark -p "$port" -q -n 1000000 -r 1000000 -c 50 -P 32 -d 16 -t set >/dev/null 2>&1
done
echo
for i in 0 1 2; do
    port=$([ $i = 0 ] && echo $rport || ([ $i = 1 ] && echo $v1port || echo $vnport))
    label=$([ $i = 0 ] && echo "redis" || ([ $i = 1 ] && echo "velt 1 thread" || echo "velt default"))
    keys=$(redis-cli -p "$port" dbsize)
    rss=$(ps -o rss= -p "${pids[$i]}" | awk '{printf "%d", $1/1024}')
    printf '%-14s %8s keys  RSS %5s MB\n' "$label" "$keys" "$rss"
done
