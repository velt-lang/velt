#!/usr/bin/env bash
# Instructions one build costs, per process kind, and velt's own split by crate (Linux).
#
#   bench/compile/features/cost.sh <velt> <std dir> <work dir> <program.velt> [velt build args...]
#
# Runs `velt build` under cachegrind with wrappers for clang, cc and ld.lld first on PATH (and
# $VELT_CLANG at the clang one), since cachegrind's --trace-children does not follow the tools
# velt starts. Build cache off and a fresh VELT_HOME, so the count is the whole build. Prints
# `velt=<n> clang=<n> cc=<n> lld=<n>` and then velt's self cost per crate (sema, lowering, VIR
# verify, optimize, Cranelift, LLVM IR text, allocation and hashing, other). Needs valgrind.
set -euo pipefail
velt=$(realpath "$1"); std=$(realpath "$2"); out=$3; prog=$4; shift 4
mkdir -p "$out/wrap"
out=$(realpath "$out")
for tool in clang cc ld.lld; do
  real=$(command -v $tool)
  printf '#!/bin/sh\nexec valgrind -q --tool=cachegrind --cache-sim=no --cachegrind-out-file=%s/%s.%%p.out %s "$@"\n' \
    "$out" "$tool" "$real" > "$out/wrap/$tool"
  chmod +x "$out/wrap/$tool"
done
export PATH=$out/wrap:$PATH VELT_CLANG=$out/wrap/clang VELT_STD=$std
export VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off VELT_HOME=$out/home
mkdir -p "$VELT_HOME"
valgrind -q --tool=cachegrind --cache-sim=no --cachegrind-out-file="$out/velt.%p.out" \
  "$velt" build "$prog" "$@" -o "$out/prog.bin" > "$out/log" 2>&1
sum() { cat "$@" 2>/dev/null | awk '/^summary:/ {s += $2} END {printf "%.0f", s}'; }
echo "velt=$(sum "$out"/velt.*.out) clang=$(sum "$out"/clang.*.out) cc=$(sum "$out"/cc.*.out) lld=$(sum "$out"/ld.lld.*.out)"
cg_annotate --auto=no --threshold=0 "$out"/velt.*.out | awk '
  /^ +[0-9,]+ +\(/ {
    n = $1; gsub(",", "", n); s = $0; c = "other"
    if (s ~ /velt_sema/) c = "sema"
    else if (s ~ /velt_vir::verify/) c = "vir-verify"
    else if (s ~ /velt_vir/) c = "lower"
    else if (s ~ /velt_opt/) c = "optimize"
    else if (s ~ /cranelift|regalloc|velt_codegen_cl/) c = "cranelift"
    else if (s ~ /velt_codegen_llvm/) c = "llvm-ir"
    else if (s ~ /mi_|malloc|free|memcpy|memmove|realloc|memset|raw_vec|hashbrown/) c = "alloc-mem-hash"
    t[c] += n
  }
  END { for (c in t) printf "%s=%.0fM\n", c, t[c] / 1e6 }' | sort | tr '\n' ' '
echo
