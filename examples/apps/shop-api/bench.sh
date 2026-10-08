#!/usr/bin/env bash
# Velt vs Node under load: starts both servers (fresh, same seed), checks that their responses
# are identical (parity.vlt), then runs each scenario against each with oha and prints
# requests/s, p50/p99 latency and the server's RSS afterwards. The Velt server uses every core
# (the default runtime); Node runs its JavaScript on one thread.
#   ./bench.sh [seconds per scenario] [connections]     (default 5 s, 64)
# Needs oha, node, curl and jq. RESULTS=<dir> keeps each run's raw oha JSON there.
set -euo pipefail
cd "$(dirname "$0")"
secs=${1:-5}
conns=${2:-64}
# A release velt: a debug one links the debug runtime, which is several times slower.
velt=${VELT:-../../../target/release/velt}
[ -x "$velt" ] || velt=velt
vport=${VELT_PORT:-18080}
nport=${NODE_PORT:-18081}
tmp=$(mktemp -d)
pids=()
last=""
cleanup() {
    for p in ${pids[@]+"${pids[@]}"}; do kill "$p" 2>/dev/null || true; done
    rm -rf "$tmp"
}
trap cleanup EXIT

"$velt" build --release -o "$tmp/shop" >/dev/null
echo "$("$velt" --version), node $(node --version), $(oha --version)"
results=${RESULTS:-}
[ -n "$results" ] && mkdir -p "$results"

start() { # start <velt|node>: a fresh server, waits until it answers
    if [ "$1" = velt ]; then
        PORT=$vport "$tmp/shop" >/dev/null &
    else
        PORT=$nport node node/server.mjs >/dev/null &
    fi
    last=$!
    pids+=("$last")
    local port=$([ "$1" = velt ] && echo $vport || echo $nport)
    for _ in $(seq 50); do
        curl -sf "http://127.0.0.1:$port/health" >/dev/null && return
        sleep 0.1
    done
    echo "$1 server did not start" >&2
    exit 1
}

stop_all() {
    for p in ${pids[@]+"${pids[@]}"}; do kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    pids=()
}

start velt
start node
"$velt" run parity.vlt -- "http://127.0.0.1:$vport" "http://127.0.0.1:$nport"
stop_all

# LOAD-1 (created before each run, stock 1000000) never runs out during a run. The bulk body's
# SKUs exist after the first request, so from then on every request decodes 200 products and
# rejects each with a conflict: the row measures decoding and validation, not creation.
order='{"customer":{"id":1,"name":"Load","email":"load@example.com"},"items":[{"sku":"LOAD-1","quantity":1}],"shipping":{"line1":"1 Main Street","city":"Oslo","postalCode":"0150","country":"NO"}}'
load_product='{"name":"Load Item","category":"toys","brand":"Acme","variants":[{"sku":"LOAD-1","color":"red","size":"M","stock":1000000,"price":{"amount":2500,"currency":"EUR"}}]}'
bulk="["
for i in $(seq 1 200); do
    [ "$i" -gt 1 ] && bulk+=","
    bulk+="{\"name\":\"Bulk $i\",\"description\":\"Imported item $i.\",\"category\":\"kitchen\",\"brand\":\"Ivy\",\"tags\":[\"imported\",\"batch\"],\"attributes\":{\"material\":\"oak\"},\"variants\":[{\"sku\":\"B-$i\",\"color\":\"red\",\"size\":\"M\",\"stock\":5,\"price\":{\"amount\":1500,\"currency\":\"EUR\"}},{\"sku\":\"B-$i-X\",\"color\":\"blue\",\"size\":\"L\",\"stock\":5,\"price\":{\"amount\":1700,\"currency\":\"EUR\"}}]}"
done
bulk+="]"
printf '%s' "$bulk" >"$tmp/bulk.json"
printf '%s' "$order" >"$tmp/order.json"

# scenario <label> <path> [method body-file]
scenarios=(
    "product by id|/products/123||"
    "list 100, sorted by price|/products?limit=100&sort=price||"
    "search + filter, 20|/products?q=waterproof&inStock=true&category=coffee||"
    "orders page of 50|/orders?status=paid&limit=50||"
    "stats over 20k orders|/stats||"
    "create order|/orders|POST|order.json"
    "bulk 200: decode + reject|/products/bulk|POST|bulk.json"
)

printf '\n%-28s %-5s %10s %9s %9s %8s\n' "scenario" "" "req/s" "p50 ms" "p99 ms" "RSS MB"
for s in "${scenarios[@]}"; do
    IFS='|' read -r label path method bodyfile <<<"$s"
    for impl in velt node; do
        start "$impl"
        port=$([ "$impl" = velt ] && echo $vport || echo $nport)
        curl -s -X POST "http://127.0.0.1:$port/products" -d "$load_product" >/dev/null
        if [ -n "$method" ]; then
            oha -z "${secs}s" -c "$conns" --no-tui --output-format json -m "$method" -D "$tmp/$bodyfile" -T application/json "http://127.0.0.1:$port$path" >"$tmp/r.json" 2>/dev/null
        else
            oha -z "${secs}s" -c "$conns" --no-tui --output-format json "http://127.0.0.1:$port$path" >"$tmp/r.json" 2>/dev/null
        fi
        rss=$(ps -o rss= -p "$last" | awk '{printf "%d", $1/1024}')
        if [ -n "$results" ]; then
            cp "$tmp/r.json" "$results/$(echo "$label" | tr -c 'a-z0-9\n' '-' | tr -s '-')-$impl.json"
        fi
        jq -r --arg l "$label" --arg i "$impl" --arg rss "$rss" \
            '"\($l)|\($i)|\(.summary.requestsPerSec|floor)|\(.latencyPercentiles.p50*1000*100|round/100)|\(.latencyPercentiles.p99*1000*100|round/100)|\($rss)|\(.statusCodeDistribution|to_entries|map("\(.key)")|join(","))"' "$tmp/r.json" |
            awk -F'|' '{printf "%-28s %-5s %10s %9s %9s %8s  %s\n", $1, $2, $3, $4, $5, $6, $7}'
        stop_all
    done
done
