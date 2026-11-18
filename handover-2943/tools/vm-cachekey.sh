#!/bin/bash
# vm-cachekey.sh <label>: (in the OrbStack VM velt-prof) the cost of velt_codegen_cl's function
# cache keys on a warm `velt build` of a float-printing hello (lazy std, the default), after a
# comment edit: callgrind, inclusive costs of codegen and of the hash functions (blake3, sha2).
# Every build writes the same output: the stamp beside it carries the files' hashes the next
# build reuses (vpm probe known_hash); a fresh output path hashes every file read again (11.6%
# of this build). Uses $HOME/tgt-7de7 (vm-build.sh). Output in prof/vm-<label>/.
source "$HOME/.cargo/env"
L=$1
R=/Users/andii/dev/velt/branches/d1-open-pr
OUT=/Users/andii/dev/velt/branches/handovers/mac-e688-2943/prof/vm-$L
V=$HOME/tgt-7de7/debug/velt
W=$HOME/vmkey
mkdir -p "$OUT"
rm -rf "$W" && mkdir -p "$W"
printf 'function main() {\n  console.log(1.5, 2.25);\n}\n' > "$W/float_hello.velt"
export VELT_STD=$R/std VELT_RT_LIB=$HOME/tgt-7de7/debug/libvelt_rt.a VELT_THREADS=1 VELT_SEMA_THREADS=1
cd "$W" || exit 1
echo "commit $(git -C "$R" rev-parse --short HEAD)" > "$OUT/README.txt"
for i in 1 2; do
  echo "// warm $RANDOM" >> float_hello.velt
  "$V" build float_hello.velt -o "$W/o" > /dev/null 2>&1
done
echo "// edit $RANDOM" >> float_hello.velt
f="$OUT/callgrind-float_hello.out"
valgrind --tool=callgrind --callgrind-out-file="$f" "$V" build float_hello.velt -o "$W/o" \
  > "$OUT/run.log" 2>&1
echo "callgrind exit $?" | tee -a "$OUT/README.txt"
callgrind_annotate --inclusive=yes "$f" > "$OUT/inclusive.txt" 2>&1
grep -m1 "PROGRAM TOTALS" "$OUT/inclusive.txt" | tee -a "$OUT/README.txt"
grep -E "velt_codegen_cl::(compile|cache)|compile_cached|function_key|blake3::|sha2::|Sha256|CacheKey" \
  "$OUT/inclusive.txt" | head -25 | tee -a "$OUT/README.txt"
rm -rf "$W"
