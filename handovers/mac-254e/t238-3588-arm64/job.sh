#!/bin/bash
# Crew #238: #3588 arm64 instructions, 4959a961a (base) vs 626d56c13 (head), one target.
set -u
W=$HOME/dev/velt/branches/t238-3588
S=$SCRATCH/t238
export CARGO_TARGET_DIR=$W/target CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
ROWS="tls_handshake tls_handshake_verified p256_verify p256_ecdh x25519"
cd "$W" || exit 1
mkdir -p "$S/bin"
for p in tls_handshake_verified p256_verify p256_ecdh x25519; do cp "$S/$p.velt" bench/crypto/; done
build() { # <side>
  echo "== build $1 at $(git log --oneline -1 | cut -c1-70) $(date +%T)"
  cargo build --release -q -p velt_rt || exit 1
  cargo build --release -q -p veltc --bin velt || exit 1
  for r in $ROWS; do
    VELT_BUILD_CACHE=off VELT_STD=$W/std VELT_RT_LIB=$W/target/release/libvelt_rt.a target/release/velt build --release "bench/crypto/$r.velt" -o "$S/bin/$r-$1" > "$S/bin/$r-$1.log" 2>&1 || { echo "$r $1 build failed"; tail -5 "$S/bin/$r-$1.log"; }
    echo "  $r $1: $("$S/bin/$r-$1" 2>&1 | tail -1)"
  done
}
build base
git checkout -q --detach 626d56c13 || exit 1
build head
instr() { VELT_THREADS=1 /usr/bin/time -l "$1" 2>&1 > /dev/null | awk '/instructions retired/ {print $1}'; }
echo "== rows $(date +%T)"
for r in $ROWS; do
  : > "$S/bin/b.txt"; : > "$S/bin/h.txt"
  for i in 1 2 3 4 5; do
    instr "$S/bin/$r-base" >> "$S/bin/b.txt"
    instr "$S/bin/$r-head" >> "$S/bin/h.txt"
  done
  b=$(sort -n "$S/bin/b.txt" | head -1); h=$(sort -n "$S/bin/h.txt" | head -1)
  bm=$(sort -n "$S/bin/b.txt" | sed -n 3p); hm=$(sort -n "$S/bin/h.txt" | sed -n 3p)
  echo "$r min base $b head $h  $(( (h - b) * 10000 / b ))bp | median base $bm head $hm  $(( (hm - bm) * 10000 / bm ))bp"
done
echo "== tests $(date +%T)"
VELT_STD=$W/std cargo test -q --release -p veltc --test p256_masks 2>&1 | grep -E 'test result|FAILED|panicked' | head
cargo test -q --release -p velt_codegen_llvm --lib field 2>&1 | grep -E 'test result|FAILED|panicked' | head
echo "=== done $(date +%T)"
