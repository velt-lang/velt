#!/usr/bin/env bash
# Compile time of spread literals and spread calls holding one long chain of operators
# (`[x + x + ..., ...ys]`, `{ ...o, b: x + ... }`, `g(x + ..., ...[1])`; `plain`: the chain in an
# array literal without a spread, for comparison), which telling the
# evaluation order once checked again per operator: `velt check` seconds per chain length,
# with every velt given.
#
#   bench/spread_order/chains.sh VELT [VELT...]      (TERMS="400 800 3000" by default)
set -euo pipefail
OUT=${TMPDIR:-/tmp}/velt-spread-chains
mkdir -p "$OUT"
for n in ${TERMS:-400 800 3000}; do
  chain=$(printf 'x + %.0s' $(seq 2 "$n"))x
  for shape in plain array object call; do
    case $shape in
      plain) line="console.log([$chain]);" ;;
      array) line="console.log([$chain, ...ys]);" ;;
      object) line="console.log({ ...o, b: $chain }.b);" ;;
      call) line="console.log(g($chain, ...[1]));" ;;
    esac
    src="$OUT/$shape-$n.ts"
    printf '%s\n' "function g(...xs: number[]): number {" "  return xs.length;" "}" \
      "let x = 1;" "const ys = [1];" "const o = { a: 1, b: 2 };" "$line" > "$src"
    for velt in "$@"; do
      TIMEFORMAT=%R
      secs=$({ time "$velt" check "$src" > /dev/null 2>&1; } 2>&1)
      printf '%s\t%s\t%s\t%s s\n' "$shape" "$n" "$velt" "$secs"
    done
  done
done
