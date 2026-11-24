#!/bin/bash
# Crew #238 part 2: four compilers (4959a961a, 626d56c13, 2fccd798e, main merge base 70bb0f8189),
# five rows, the tests at 2fccd798e. Binaries <row>-{base,h626,h2fc,main}; one target.
set -u
W=$HOME/dev/velt/branches/t238-3588
S=$SCRATCH/t238
B=$S/bin
export CARGO_TARGET_DIR=$W/target CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
ROWS="tls_handshake tls_handshake_verified p256_verify p256_ecdh x25519"
cd "$W" || exit 1
for p in tls_handshake_verified p256_verify p256_ecdh x25519; do cp "$S/$p.velt" bench/crypto/; done
# The first job's head binaries are 626d56c13's.
for r in $ROWS; do [ -x "$B/$r-head" ] && mv "$B/$r-head" "$B/$r-h626"; done
row() { # <row> <side>
  VELT_BUILD_CACHE=off VELT_STD=$W/std VELT_RT_LIB=$W/target/release/libvelt_rt.a target/release/velt build --release "bench/crypto/$1.velt" -o "$B/$1-$2" > "$B/$1-$2.log" 2>&1 || { echo "$1 $2 build failed"; tail -5 "$B/$1-$2.log"; return; }
  echo "  $1 $2: $("$B/$1-$2" 2>&1 | tail -1)"
}
at() { # <ref> <side> <rt?> <rows>
  git checkout -q --detach "$1" || exit 1
  echo "== $2 at $(git log --oneline -1 | cut -c1-60) $(date +%T)"
  [ "$3" = rt ] && { cargo build --release -q -p velt_rt || exit 1; }
  cargo build --release -q -p veltc --bin velt || exit 1
  for r in $4; do row "$r" "$2"; done
}
at 626d56c13 h626 no "p256_verify"
at 4959a961a base no "p256_verify"
at 2fccd798e h2fc no "$ROWS"
echo "== tests at 2fccd798e $(date +%T)"
VELT_STD=$W/std cargo test -q --release -p veltc --test p256_masks 2>&1 | grep -E 'test result|FAILED|panicked' | head
cargo test -q --release -p velt_codegen_llvm --lib field 2>&1 | grep -E 'test result|FAILED|panicked|arm64_choice' | head
cargo test -q --release -p velt_codegen_llvm --lib the_arm64_choice_is_csels_in_assembly -- --nocapture 2>&1 | grep -E 'test result|FAILED|panicked|running' | head
at 70bb0f8189 main rt "$ROWS"
instr() { VELT_THREADS=1 /usr/bin/time -l "$1" 2>&1 > /dev/null | awk '/instructions retired/ {print $1}'; }
echo "== rows $(date +%T)"
for r in $ROWS; do
  for s in base h626 h2fc main; do : > "$B/$s.txt"; done
  for i in 1 2 3 4 5; do
    for s in base h626 h2fc main; do
      [ -x "$B/$r-$s" ] && instr "$B/$r-$s" >> "$B/$s.txt"
    done
  done
  line="$r"
  for s in base h626 h2fc main; do
    line="$line | $s min $(sort -n "$B/$s.txt" | head -1) med $(sort -n "$B/$s.txt" | sed -n 3p)"
  done
  echo "$line"
done
echo "=== done $(date +%T)"
