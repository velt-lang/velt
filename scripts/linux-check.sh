#!/usr/bin/env bash
# Run scripts/check-all.sh on Linux inside a Docker container, from macOS or Linux (Docker
# Desktop, OrbStack or colima). The toolchain image comes from scripts/linux/Dockerfile.<distro>
# (Debian 12 / glibc by default, Alpine / musl with --distro alpine) and is built per --platform:
# on Apple silicon linux/arm64 runs natively and linux/amd64 under Rosetta/QEMU emulation.
#
# The working tree (tracked + untracked, not ignored files; uncommitted changes included) is
# copied into the container, so the host's target/ is never shared. Cargo's registry and the
# container's target/ live in named volumes (velt-cargo-<arch>, velt-target-<distro>-<arch>),
# which makes re-runs incremental; `docker volume rm` them to reclaim the space.
#
# Usage: scripts/linux-check.sh [--platform linux/arm64|linux/amd64] [--distro bookworm|alpine]
#                              [--services] [--shell | --run '<command>'] [-- <check-all args>]
#   --platform  container platform (default: the Docker host's)
#   --services  also start PostgreSQL 17 and Redis 7 (plain + TLS) containers on a private
#               network and export VELT_TEST_PG_URL, VELT_TEST_REDIS_URL, VELT_TEST_REDISS_URL (+ _CA),
#               so the database goldens run too
#   --shell     open an interactive shell in the prepared container instead of check-all
#   --run CMD   run CMD (bash -c) in the prepared container instead of check-all
# Examples:
#   scripts/linux-check.sh --platform linux/arm64 --services
#   scripts/linux-check.sh --platform linux/amd64 -- --no-smoke
#   scripts/linux-check.sh --distro alpine --run 'cargo build -p veltc && target/debug/velt doctor'
set -euo pipefail

platform=""
distro=bookworm
services=0
mode=check
run_cmd=""
while [ $# -gt 0 ]; do
    case "$1" in
        --platform) platform=$2; shift 2 ;;
        --distro) distro=$2; shift 2 ;;
        --services) services=1; shift ;;
        --shell) mode=shell; shift ;;
        --run) mode=run; run_cmd=$2; shift 2 ;;
        --) shift; break ;;
        *) echo "unknown option: $1 (see the header of $0)" >&2; exit 2 ;;
    esac
done

repo=$(cd "$(dirname "$0")/.." && pwd)
dockerfile="$repo/scripts/linux/Dockerfile.$distro"
if [ ! -f "$dockerfile" ]; then
    echo "error: no $dockerfile (distros: bookworm, alpine)" >&2
    exit 2
fi
if ! command -v docker >/dev/null 2>&1; then
    echo "error: docker not found; install Docker Desktop, OrbStack or colima" >&2
    exit 1
fi
if [ -z "$platform" ]; then
    case "$(docker version --format '{{.Server.Arch}}')" in
        arm64 | aarch64) platform=linux/arm64 ;;
        *) platform=linux/amd64 ;;
    esac
fi
arch=${platform#linux/}
image="velt-linux-$distro:$arch"
network="velt-linux-check-$arch"

step() {
    printf '\033[36m==> %s\033[0m\n' "$*"
}

step "toolchain image $image"
bash "$repo/scripts/linux/build-image.sh" "$platform" "$image" "$dockerfile"

service_containers=()
cleanup() {
    for c in "${service_containers[@]:-}"; do
        [ -n "$c" ] && docker rm -f -v "$c" >/dev/null 2>&1 || true
    done
    docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT

env_args=()
net_args=()
if [ "$services" = 1 ]; then
    step "services: PostgreSQL 17, Redis 7 (plain and TLS)"
    docker network create "$network" >/dev/null 2>&1 || true
    net_args=(--network "$network" -v "velt-tls-$arch:/tls:ro")
    pg="velt-pg-$arch"
    redis="velt-redis-$arch"
    rediss="velt-rediss-$arch"
    docker rm -f -v "$pg" "$redis" "$rediss" >/dev/null 2>&1 || true
    # A throwaway CA + server certificate for the TLS Redis (SAN = its container name); the
    # test container trusts it through VELT_TEST_REDISS_CA.
    docker run --rm --platform "$platform" -v "velt-tls-$arch:/tls" "$image" bash -c "set -e
cd /tls
openssl req -x509 -newkey rsa:2048 -nodes -days 30 -subj /CN=velt-test-ca \
    -keyout ca.key -out ca.crt 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj /CN=$rediss -keyout server.key -out server.csr 2>/dev/null
printf 'subjectAltName=DNS:$rediss\n' > san.ext
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 30 \
    -extfile san.ext -out server.crt 2>/dev/null
chmod 644 /tls/*" || {
        echo "error: could not create the TLS test certificate (openssl in $image?)" >&2
        exit 1
    }
    docker run -d --name "$pg" --network "$network" --platform "$platform" \
        -e POSTGRES_PASSWORD=velt -e POSTGRES_DB=velt_test postgres:17 >/dev/null
    service_containers+=("$pg")
    docker run -d --name "$redis" --network "$network" --platform "$platform" redis:7 >/dev/null
    service_containers+=("$redis")
    docker run -d --name "$rediss" --network "$network" --platform "$platform" \
        -v "velt-tls-$arch:/tls:ro" redis:7 redis-server --port 0 --tls-port 6380 \
        --tls-cert-file /tls/server.crt --tls-key-file /tls/server.key \
        --tls-ca-cert-file /tls/ca.crt --tls-auth-clients no >/dev/null
    service_containers+=("$rediss")
    for _ in $(seq 1 60); do
        docker exec "$pg" pg_isready -q -U postgres >/dev/null 2>&1 && break
        sleep 1
    done
    env_args+=(-e "VELT_TEST_PG_URL=postgres://postgres:velt@$pg/velt_test"
        -e "VELT_TEST_REDIS_URL=redis://$redis:6379"
        -e "VELT_TEST_REDISS_URL=rediss://$rediss:6380"
        -e "VELT_TEST_REDISS_CA=/tls/ca.crt")
fi

case "$mode" in
    check) inner="bash scripts/check-all.sh $*" ;;
    shell) inner="bash" ;;
    run) inner="$run_cmd" ;;
esac
# The tree goes in as a tarball (stdin stays free for --shell). CARGO_TARGET_DIR is outside
# /work so the copied tree never contains it; CARGO_PROFILE_DEV_DEBUG keeps it small (line
# tables are enough for backtraces).
work=$(mktemp -d)
trap 'cleanup; rm -rf "$work"' EXIT
# COPYFILE_DISABLE, --no-xattrs: macOS tar would otherwise add AppleDouble `._*` files and xattr
# headers (provenance) that GNU tar warns about.
(cd "$repo" && git ls-files -z --cached --others --exclude-standard |
    COPYFILE_DISABLE=1 tar --no-xattrs -cf "$work/tree.tar" --null -T - 2>/dev/null) || true
script="set -euo pipefail
find /work -mindepth 1 -delete
tar -xf /tree.tar -C /work
cd /work
echo \"container: \$(uname -sm), \$(ldd --version 2>&1 | head -1), \$(rustc --version)\"
$inner"

tty_args=(-i)
[ "$mode" = shell ] && tty_args=(-it)
step "$mode in $image ($platform)"
docker run --rm "${tty_args[@]}" --platform "$platform" ${net_args[@]+"${net_args[@]}"} \
    -v "$work/tree.tar:/tree.tar:ro" \
    -v "velt-cargo-$arch:/opt/cargo/registry" \
    -v "velt-target-$distro-$arch:/target" \
    -e CARGO_TARGET_DIR=/target -e CARGO_INCREMENTAL=0 \
    -e CARGO_PROFILE_DEV_DEBUG=line-tables-only \
    ${env_args[@]+"${env_args[@]}"} "$image" bash -c "$script"
