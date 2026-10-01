#!/usr/bin/env bash
# Database client benchmark runner (macOS / Linux): Velt std/sqlite, std/redis, std/postgres
# vs Node (better-sqlite3, ioredis, pg) and Rust (rusqlite, redis, tokio-postgres). Method: README.md.
#
#   bench/db/run.sh [--quick] [--runs N] [workload...]
#
#   --quick     1/20 of the sizes (a correctness pass)
#   --runs N    timed runs per implementation (default 3); each workload keeps its best ops/s
#   workload    backend (`sqlite`, `redis`, `postgres`) or `backend.workload` (`redis.get_seq`)
#
# Servers: Postgres from $BENCH_PG_URL or $VELT_TEST_PG_URL, Redis from $BENCH_REDIS_URL or
# $VELT_TEST_REDIS_URL; a backend without a URL is skipped (n/a). Every program first runs once
# untimed, and each workload's op count and checksum must match Rust's. Velt is built with
# `--release` on both backends (LLVM and Cranelift), Rust from rust/ (release, LTO), Node deps
# with `npm ci` into node/node_modules. Build outputs go to target/bench-db (the Velt compiler
# itself to $CARGO_TARGET_DIR or target/). Needs cargo, node + npm, python3.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT="$ROOT/target/bench-db"
QUICK=0
RUNS=3
FILTERS=()
while [[ $# -gt 0 ]]; do
  case $1 in
    --quick) QUICK=1 ;;
    --runs) RUNS=$2; shift ;;
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
    *) FILTERS+=("$1") ;;
  esac
  shift
done
mkdir -p "$OUT/bin"

export BENCH_PG_URL=${BENCH_PG_URL:-${VELT_TEST_PG_URL:-}}
export BENCH_REDIS_URL=${BENCH_REDIS_URL:-${VELT_TEST_REDIS_URL:-}}
export VELT_STD=${VELT_STD:-$ROOT/std}

# wanted <backend>: whether any filter selects this backend (no filters: all).
wanted() {
  [[ ${#FILTERS[@]} -eq 0 ]] && return 0
  local f
  for f in ${FILTERS[@]+"${FILTERS[@]}"}; do [[ ${f%%.*} == "$1" ]] && return 0; done
  return 1
}

BACKENDS=()
for b in sqlite redis postgres; do wanted "$b" && BACKENDS+=("$b"); done

echo "building velt (release) and the runtime..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
VELT="${CARGO_TARGET_DIR:-$ROOT/target}/release/velt"
echo "building the Rust baselines..." >&2
RUST_BIN=""
if cargo build --release -q --manifest-path "$HERE/rust/Cargo.toml" --target-dir "$OUT/rust-target" >&2; then
  RUST_BIN="$OUT/rust-target/release"
fi
NODE_OK=0
if command -v node > /dev/null && command -v npm > /dev/null; then
  if [[ -d "$HERE/node/node_modules" ]] || (cd "$HERE/node" && npm ci --no-audit --no-fund >&2); then
    NODE_OK=1
  fi
fi

# impls <backend>: lines "label|command" (command "n/a: <reason>" when it can't run).
impls() {
  local b=$1 kind exe
  if [[ -n $RUST_BIN ]]; then
    echo "Rust|$RUST_BIN/$b"
    [[ $b != sqlite ]] && echo "Rust current-thread|$RUST_BIN/$b current"
  else
    echo "Rust|n/a: cargo build of bench/db/rust failed"
  fi
  for kind in llvm cranelift; do
    label="Velt LLVM"
    [[ $kind == cranelift ]] && label="Velt Cranelift"
    exe="$OUT/bin/$b-$kind"
    if "$VELT" build --release --backend "$kind" "$HERE/velt/$b.vlt" -o "$exe" >&2; then
      echo "$label|$exe"
    else
      echo "$label|n/a: velt build failed"
    fi
  done
  if [[ $NODE_OK -eq 1 ]]; then
    echo "Node|node $HERE/node/$b.mjs"
  else
    echo "Node|n/a: node/npm missing or npm ci failed"
  fi
}

LIST="$OUT/impls.txt"
: > "$LIST"
for b in "${BACKENDS[@]}"; do
  case $b in
    postgres) url=$BENCH_PG_URL var=PG_URL ;;
    redis) url=$BENCH_REDIS_URL var=REDIS_URL ;;
    *) url=local var="" ;;
  esac
  if [[ -z $url ]]; then
    echo "$b|all|n/a: no server (set BENCH_$var or VELT_TEST_$var)" >> "$LIST"
    continue
  fi
  impls "$b" | sed "s/^/$b|/" >> "$LIST"
done

python3 "$HERE/measure.py" "$LIST" "$RUNS" "$QUICK" ${FILTERS[@]+"${FILTERS[@]}"}
