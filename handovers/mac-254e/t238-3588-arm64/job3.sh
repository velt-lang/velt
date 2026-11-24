#!/bin/bash
# Crew #240: #3588 at 37ce5916b against main 70bb0f818 (its binaries kept from #238), five rows,
# then p256_masks and the codegen field tests at 37ce5916b. Each run under a timeout, to files.
set -u
W=$HOME/dev/velt/branches/t240-3588
S=$SCRATCH/t238
B=$S/bin
export CARGO_TARGET_DIR=$W/target CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
ROWS="tls_handshake tls_handshake_verified p256_verify p256_ecdh x25519"
to() { perl -e 'alarm shift; exec @ARGV' "$@"; }
cd "$W" || exit 1
echo "== build at $(git log --oneline -1 | cut -c1-70) $(date +%T)"
for p in tls_handshake_verified p256_verify p256_ecdh x25519; do cp "$S/$p.velt" bench/crypto/; done
cargo build --release -q -p velt_rt || exit 1
cargo build --release -q -p veltc --bin velt || exit 1
for r in $ROWS; do
  VELT_BUILD_CACHE=off VELT_STD=$W/std VELT_RT_LIB=$W/target/release/libvelt_rt.a to 600 target/release/velt build --release "bench/crypto/$r.velt" -o "$B/$r-h37c" > "$B/$r-h37c.log" 2>&1 || { echo "$r build failed"; tail -5 "$B/$r-h37c.log"; continue; }
  to 120 "$B/$r-h37c" > "$B/$r-h37c.out" 2>&1
  echo "  $r h37c: $(tail -1 "$B/$r-h37c.out") | main: $(to 120 "$B/$r-main" 2>&1 | tail -1)"
done
instr() { VELT_THREADS=1 to 120 /usr/bin/time -l "$1" 2> "$B/time.txt" > /dev/null; awk '/instructions retired/ {print $1}' "$B/time.txt"; }
echo "== rows $(date +%T)"
for r in $ROWS; do
  : > "$B/m.txt"; : > "$B/h.txt"
  for i in 1 2 3 4 5; do
    instr "$B/$r-main" >> "$B/m.txt"
    instr "$B/$r-h37c" >> "$B/h.txt"
  done
  mn=$(sort -n "$B/m.txt" | head -1); hn=$(sort -n "$B/h.txt" | head -1)
  mm=$(sort -n "$B/m.txt" | sed -n 3p); hm=$(sort -n "$B/h.txt" | sed -n 3p)
  echo "$r | main min $mn med $mm | h37c min $hn med $hm | min $(( (hn - mn) * 10000 / mn ))bp med $(( (hm - mm) * 10000 / mm ))bp"
done
echo "== tests $(date +%T)"
VELT_STD=$W/std to 1800 cargo test -q --release -p veltc --test p256_masks > "$S/t240-masks.log" 2>&1
grep -E 'test result|FAILED|panicked' "$S/t240-masks.log" | head
to 1800 cargo test -q --release -p velt_codegen_llvm --lib field > "$S/t240-field.log" 2>&1
grep -E 'test result|FAILED|panicked' "$S/t240-field.log" | head
grep -c "the_arm64_choice\|arm64" "$S/t240-field.log"
echo "=== done $(date +%T)"
