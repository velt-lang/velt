#!/usr/bin/env bash
# Run bench/web/run.sh on Linux in Docker (from macOS or Linux): the servers and wrk in one
# container built from scripts/linux/Dockerfile.bench (the scripts/linux-check.sh toolchain image
# plus Node, Bun, Go and wrk), Postgres with the TFB schema in another, on a private network.
# The working tree is copied in (as scripts/linux-check.sh does); results land in
# bench/web/results/linux-<arch>-<date>.{md,jsonl}.
#
# Usage: bench/web/linux.sh [--platform linux/arm64|linux/amd64] [-- VAR=value ...]
#   Everything after `--` is passed to run.sh as environment (QUICK=1, SERVERS="velt rust", ...).
# The velt compiler and runtime are built in release mode (a debug `velt` links the debug
# runtime) into the linux-check target volume, which keeps rebuilds incremental.
set -euo pipefail

platform=""
while [ $# -gt 0 ]; do
    case "$1" in
        --platform) platform=$2; shift 2 ;;
        --) shift; break ;;
        *) echo "unknown option: $1 (see the header of $0)" >&2; exit 2 ;;
    esac
done

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
if [ -z "$platform" ]; then
    case "$(docker version --format '{{.Server.Arch}}')" in
        arm64 | aarch64) platform=linux/arm64 ;;
        *) platform=linux/amd64 ;;
    esac
fi
arch=${platform#linux/}
network="velt-web-$arch"
pg="velt-web-pg-$arch"

echo "==> images"
bash "$repo/scripts/linux/build-image.sh" "$platform" "velt-linux-bookworm:$arch" \
    "$repo/scripts/linux/Dockerfile.bookworm"
bash "$repo/scripts/linux/build-image.sh" "$platform" "velt-web-bench:$arch" \
    "$repo/scripts/linux/Dockerfile.bench" --build-arg "BASE=velt-linux-bookworm:$arch"

work=$(mktemp -d)
cleanup() {
    docker rm -f -v "$pg" >/dev/null 2>&1 || true
    docker network rm "$network" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

echo "==> postgres ($pg)"
docker network create "$network" >/dev/null 2>&1 || true
docker rm -f -v "$pg" >/dev/null 2>&1 || true
# The same settings as db/db.sh.
docker run -d --name "$pg" --network "$network" --platform "$platform" --shm-size=1g \
    -e POSTGRES_USER=benchmarkdbuser -e POSTGRES_PASSWORD=benchmarkdbpass \
    -e POSTGRES_DB=hello_world -v "$here/db/init.sql:/docker-entrypoint-initdb.d/init.sql:ro" \
    postgres:17 \
    -c max_connections=2000 -c shared_buffers=256MB -c effective_cache_size=1GB \
    -c synchronous_commit=off -c checkpoint_timeout=15min -c max_wal_size=4GB \
    -c work_mem=16MB -c max_prepared_transactions=0 >/dev/null
for _ in $(seq 1 120); do
    if docker exec "$pg" pg_isready -h 127.0.0.1 -U benchmarkdbuser -d hello_world >/dev/null 2>&1 &&
        docker exec "$pg" psql -h 127.0.0.1 -U benchmarkdbuser -d hello_world -tAc \
            "SELECT count(*) FROM world" 2>/dev/null | grep -q 10000; then
        break
    fi
    sleep 1
done

# COPYFILE_DISABLE: no AppleDouble `._*` files from macOS tar.
(cd "$repo" && git ls-files -z --cached --others --exclude-standard |
    COPYFILE_DISABLE=1 tar --no-xattrs -cf "$work/tree.tar" --null -T - 2>/dev/null) || true
mkdir -p "$here/results"
out="/results/linux-$arch-$(date +%Y-%m-%d-%H%M)"
env_args=()
for kv in "$@"; do
    env_args+=(-e "$kv")
done
script="set -euo pipefail
mkdir -p /work && tar -xf /tree.tar -C /work && cd /work
echo \"container: \$(uname -sm), \$(nproc) cpus, node \$(node --version), bun \$(bun --version), \$(go version), \$(rustc --version)\"
cargo build -q --release -p veltc -p velt_rt
VELT=/target/release/velt RUST_TARGET_DIR=/target/web-rust OUT=$out bash bench/web/run.sh"
echo "==> bench in velt-web-bench:$arch"
docker run --rm -i --platform "$platform" --network "$network" \
    -v "$work/tree.tar:/tree.tar:ro" -v "$here/results:/results" \
    -v "velt-cargo-$arch:/opt/cargo/registry" -v "velt-target-bookworm-$arch:/target" \
    -e CARGO_TARGET_DIR=/target -e CARGO_INCREMENTAL=0 -e CARGO_PROFILE_DEV_DEBUG=line-tables-only \
    -e "DATABASE_URL=postgres://benchmarkdbuser:benchmarkdbpass@$pg:5432/hello_world" \
    ${env_args[@]+"${env_args[@]}"} "velt-web-bench:$arch" bash -c "$script"
