#!/usr/bin/env bash
# Postgres 17 with the TechEmpower schema in Docker, for bench/web.
# Usage: bench/web/db/db.sh up|down|status|psql
# Knobs: PGPORT (5432; published on 127.0.0.1 only), PG_CONTAINER (velt-web-pg),
#        PG_IMAGE (postgres:17). `up` recreates the container, so the data starts fresh.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
PGPORT="${PGPORT:-5432}"
NAME="${PG_CONTAINER:-velt-web-pg}"
IMAGE="${PG_IMAGE:-postgres:17}"

port_busy() {
  if command -v lsof >/dev/null 2>&1; then
    lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1
  else
    (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null
  fi
}

up() {
  docker rm -f -v "$NAME" >/dev/null 2>&1 || true
  if port_busy "$PGPORT"; then
    echo "port $PGPORT is already in use (a local postgres?); rerun with PGPORT=5433" >&2
    exit 1
  fi
  # TFB-like server settings: many connections, commits that don't wait for fsync.
  docker run -d --name "$NAME" --shm-size=1g \
    -p "127.0.0.1:$PGPORT:5432" \
    -e POSTGRES_USER=benchmarkdbuser -e POSTGRES_PASSWORD=benchmarkdbpass \
    -e POSTGRES_DB=hello_world \
    -v "$HERE/init.sql:/docker-entrypoint-initdb.d/init.sql:ro" \
    "$IMAGE" \
    -c max_connections=2000 -c shared_buffers=256MB -c effective_cache_size=1GB \
    -c synchronous_commit=off -c checkpoint_timeout=15min -c max_wal_size=4GB \
    -c work_mem=16MB -c max_prepared_transactions=0 >/dev/null
  # The entrypoint runs init.sql against a temporary server first; wait for the real one
  # (a TCP connection through the published port works only once it is up).
  for _ in $(seq 1 120); do
    if docker exec "$NAME" pg_isready -h 127.0.0.1 -U benchmarkdbuser -d hello_world >/dev/null 2>&1 &&
      [ "$(docker exec "$NAME" psql -h 127.0.0.1 -U benchmarkdbuser -d hello_world -tAc \
        'SELECT count(*) FROM world' 2>/dev/null)" = "10000" ]; then
      echo "postgres up: postgres://benchmarkdbuser:benchmarkdbpass@127.0.0.1:$PGPORT/hello_world"
      return 0
    fi
    sleep 0.5
  done
  echo "postgres did not come up; docker logs $NAME" >&2
  exit 1
}

case "${1:-}" in
  up) up ;;
  down) docker rm -f -v "$NAME" >/dev/null 2>&1 || true ;;
  status) docker ps --filter "name=^${NAME}$" ;;
  psql) shift; docker exec -i "$NAME" psql -U benchmarkdbuser -d hello_world "$@" ;;
  *) echo "usage: $0 up|down|status|psql [args]" >&2; exit 2 ;;
esac
