#!/usr/bin/env bash
# f16x8's arithmetic against Rust and C (half_arith.velt, half_arith_rust/, half_arith.c), arm64
# only: instructions retired per element, each kernel run with 0 and with PASSES passes and
# subtracted. Never wall clock.
#
# The native rows need Rust nightly, as stable has no f16 NEON intrinsics; its date is pinned here.
# The widened rows are stable Rust (rust-toolchain.toml) and `half`. C is a check on both. A Velt
# build for `--cpu cortex-a53`, which has no FP16, is the path without it, and runs on any arm64.
#
#   rustup toolchain install nightly-2026-09-30 --profile minimal
#   VELT=target/release/velt bench/simd/half_arith.sh [passes]
set -euo pipefail
NIGHTLY=nightly-2026-09-30
PASSES=${1:-100}
RUNS=${RUNS:-5}
N=1048576
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
VELT=${VELT:-$ROOT/target/release/velt}
OUT=${OUT:-$ROOT/target/bench-half-arith}
mkdir -p "$OUT"
"$VELT" build --release --backend llvm "$HERE/half_arith.velt" -o "$OUT/velt"
"$VELT" build --release --backend llvm --cpu cortex-a53 "$HERE/half_arith.velt" -o "$OUT/velt-soft"
clang -O2 -mcpu=apple-m1 -ffp-contract=off "$HERE/half_arith.c" -o "$OUT/c"
RUSTFLAGS="-C target-cpu=apple-m1" CARGO_TARGET_DIR="$OUT/rust-native" \
  cargo "+$NIGHTLY" build --release -q --features native \
  --manifest-path "$HERE/half_arith_rust/Cargo.toml"
RUSTFLAGS="-C target-cpu=apple-m1" CARGO_TARGET_DIR="$OUT/rust-stable" \
  cargo build --release -q --manifest-path "$HERE/half_arith_rust/Cargo.toml"
native=$OUT/rust-native/release/half_arith
stable=$OUT/rust-stable/release/half_arith

instr() { # <binary> <kernel> <passes>: the fewest of $RUNS runs (the runtime's threads add noise)
  for _ in $(seq "$RUNS"); do
    /usr/bin/time -l "$@" 2>&1 >/dev/null | awk '/instructions retired/ {print $1}'
  done | sort -n | head -1
}

per() { # <binary> <kernel>: instructions per element
  local base full
  base=$(instr "$1" "$2" 0)
  full=$(instr "$1" "$2" "$PASSES")
  awk -v f="$full" -v b="$base" -v p="$PASSES" -v n="$N" 'BEGIN { printf "%.2f", (f - b) / p / n }'
}

echo "rust: $(rustc "+$NIGHTLY" --version) and $(rustc --version); clang: $(clang --version | head -1)"
printf '| kernel | Velt | Rust nightly | C | Velt / Rust | checksums |\n|---|---:|---:|---:|---:|---|\n'
for k in axpy act dot; do
  v=$(per "$OUT/velt" "$k")
  r=$(per "$native" "$k")
  c=$(per "$OUT/c" "$k")
  same=differ
  a=$("$OUT/velt" "$k" 3) b=$("$native" "$k" 3) d=$("$OUT/c" "$k" 3) e=$("$OUT/velt-soft" "$k" 3)
  [ "$a" = "$b" ] && [ "$a" = "$d" ] && [ "$a" = "$e" ] && same=same
  awk -v k="$k" -v v="$v" -v r="$r" -v c="$c" -v s="$same" \
    'BEGIN { printf "| %s | %.2f | %.2f | %.2f | %.2f | %s |\n", k, v, r, c, v / r, s }'
done
printf '\nWithout FP16 (`--cpu cortex-a53`), against stable Rust with `half`:\n\n'
printf '| kernel | Velt `mulAdd` | Velt `mulAddFast` | Rust + half | C | `mulAddFast` / Rust | checksums |\n|---|---:|---:|---:|---:|---:|---|\n'
for k in axpy act dot; do
  e=$(per "$OUT/velt-soft" "$k")
  f=$(per "$OUT/velt-soft" "${k}_fast")
  r=$(per "$stable" "$k")
  c=$(per "$OUT/c" "${k}_wide")
  same=differ
  a=$("$OUT/velt-soft" "${k}_fast" 3) b=$("$stable" "$k" 3) d=$("$OUT/c" "${k}_wide" 3)
  [ "$a" = "$b" ] && [ "$a" = "$d" ] && same=same
  awk -v k="$k" -v e="$e" -v f="$f" -v r="$r" -v c="$c" -v s="$same" \
    'BEGIN { printf "| %s | %.2f | %.2f | %.2f | %.2f | %.2f | %s |\n", k, e, f, r, c, f / r, s }'
done
