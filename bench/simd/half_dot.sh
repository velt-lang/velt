#!/usr/bin/env bash
# The f16 and bf16 dot products against Rust (half_dot.velt, half_dot_rust/), arm64 only: instructions
# retired per 1M weights, each kernel run with 0 and with PASSES passes and subtracted. Never wall
# clock. Set VELT_CLANG to a clang as new as rustc's LLVM. bf16's fast kernels are built with
# `--cpu $FAST_CPU` (default apple-m2), which needs a CPU with FEAT_BF16 to run.
#
# Two Velt columns, because Rust's kernels are `#[inline(never)]`. "Velt out of line"
# (half_dot_far.velt) is the fair comparison; "Velt inlined" (half_dot.velt) lets LLVM see in `main`
# that both arrays have one length. The explicit two-array `fits` kernels have one exit test
# either way because velt_opt's `fits_pair` makes `fits(a, j) && fits(b, j)` one test; without it,
# out of line, they were 1.06 to 1.65 times Rust.
#
#   VELT=target/release/velt VELT_CLANG=/opt/homebrew/opt/llvm/bin/clang bench/simd/half_dot.sh [passes]
set -euo pipefail
PASSES=${1:-100}
RUNS=${RUNS:-5}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
VELT=${VELT:-$ROOT/target/release/velt}
OUT=${OUT:-$ROOT/target/bench-half-dot}
FAST_CPU=${FAST_CPU:-apple-m2}
mkdir -p "$OUT"
"$VELT" build --release --backend llvm "$HERE/half_dot.velt" -o "$OUT/velt"
"$VELT" build --release --backend llvm --cpu "$FAST_CPU" "$HERE/half_dot.velt" -o "$OUT/velt-fast"
"$VELT" build --release --backend llvm "$HERE/half_dot_far.velt" -o "$OUT/far"
"$VELT" build --release --backend llvm --cpu "$FAST_CPU" "$HERE/half_dot_far.velt" -o "$OUT/far-fast"
RUSTC_BOOTSTRAP=1 RUSTFLAGS="-C target-cpu=apple-m1" CARGO_TARGET_DIR="$OUT/rust-target" \
  cargo build --release -q --manifest-path "$HERE/half_dot_rust/Cargo.toml"
rust=$OUT/rust-target/release/half_dot

instr() { # <binary> <kernel> <passes>: the fewest of $RUNS runs (the runtime's threads add noise)
  for _ in $(seq "$RUNS"); do
    /usr/bin/time -l "$@" 2>&1 >/dev/null | awk '/instructions retired/ {print $1}'
  done | sort -n | head -1
}

printf '| kernel | Velt inlined | Velt out of line | Rust | inlined / Rust | out of line / Rust | checksums |\n'
printf '|---|---:|---:|---:|---:|---:|---|\n'
for kernel in f16_one f16_four f16_bulk bf16_fast_one bf16_fast_four bf16_one bf16_four bf16_bulk i8_bulk; do
  v=$OUT/velt
  f=$OUT/far
  case $kernel in bf16_fast_*) v=$OUT/velt-fast f=$OUT/far-fast ;; esac
  case $kernel in i8_*) f=$v ;; esac # `dotI8` is `noInline` in std and has one form
  base_v=$(instr "$v" "$kernel" 0)
  full_v=$(instr "$v" "$kernel" "$PASSES")
  base_f=$(instr "$f" "$kernel" 0)
  full_f=$(instr "$f" "$kernel" "$PASSES")
  base_r=$(instr "$rust" "$kernel" 0)
  full_r=$(instr "$rust" "$kernel" "$PASSES")
  same=$([ "$("$v" "$kernel" 1)" = "$("$rust" "$kernel" 1)" ] && [ "$("$f" "$kernel" 1)" = "$("$rust" "$kernel" 1)" ] && echo same || echo differ)
  awk -v k="$kernel" -v fv="$full_v" -v bv="$base_v" -v ff="$full_f" -v bf="$base_f" -v fr="$full_r" -v br="$base_r" -v p="$PASSES" -v s="$same" \
    'BEGIN { v = (fv - bv) / p; f = (ff - bf) / p; r = (fr - br) / p
             printf "| %s | %.0f | %.0f | %.0f | %.3f | %.3f | %s |\n", k, v, f, r, v / r, f / r, s }'
done
