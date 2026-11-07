#!/usr/bin/env bash
# Velt against Rust on struct-heavy programs (#641): each <name>.velt next to <name>.rs, the same
# algorithm with a `struct` in Velt and a `#[derive(Clone, Copy)]` struct in Rust.
#
#   bench/structs/ab/run.sh [--velt PATH] [--out DIR]
#
# Checks that both print the same output, then prints a Markdown table of the instructions each
# executes: valgrind cachegrind on Linux (VELT_THREADS=1), `/usr/bin/time -l`'s "instructions
# retired" on macOS. Both include process start-up (an empty program: about 0.1M instructions
# under cachegrind, 16-19M on macOS). --velt: a release `velt` (default: build one).
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
VELT=
OUT=
while [ $# -gt 0 ]; do
  case "$1" in
    --velt) VELT=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    *) echo "usage: bench/structs/ab/run.sh [--velt PATH] [--out DIR]" >&2; exit 2 ;;
  esac
done
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
OUT=${OUT:-$TARGET/bench-structs-ab}
mkdir -p "$OUT"
if [ -z "$VELT" ]; then
  echo "building velt (release) and the runtime..." >&2
  cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
  VELT="$TARGET/release/velt"
fi

count() {
  if [ "$(uname)" = Darwin ]; then
    /usr/bin/time -l "$1" 2>&1 >/dev/null | awk '/instructions retired/ { print $1 }'
  else
    VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
      "$1" 2>&1 >/dev/null | awk '/I *refs:/ { gsub(",", "", $NF); print $NF }'
  fi
}

echo "| program | Velt | Rust | Velt / Rust |"
echo "|---|---:|---:|---:|"
for src in "$HERE"/*.velt; do
  name=$(basename "$src" .velt)
  "$VELT" build --release "$src" -o "$OUT/$name-velt" >&2
  rustc -C opt-level=3 -o "$OUT/$name-rust" "$HERE/$name.rs" >&2
  a=$("$OUT/$name-velt")
  b=$("$OUT/$name-rust")
  if [ "$a" != "$b" ]; then
    echo "$name: outputs differ: velt '$a', rust '$b'" >&2
    exit 1
  fi
  v=$(count "$OUT/$name-velt")
  r=$(count "$OUT/$name-rust")
  awk -v n="$name" -v v="$v" -v r="$r" \
    'BEGIN { printf "| %s | %.1fM | %.1fM | %.2f |\n", n, v / 1e6, r / 1e6, v / r }'
done
