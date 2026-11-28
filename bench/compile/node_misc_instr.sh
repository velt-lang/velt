#!/usr/bin/env bash
# Instructions (cachegrind, one thread) for the rows of #3978 / #3884 / #3965: two compilers
# lowering hello and a generated 10,000-line application to VIR (`build --emit=vir`: no backend,
# no linker), and the --release programs of the area run: a URL parse loop (domains, IPv4, IPv6
# hosts), proxy member reads and `typeof` in a program that has `Proxy` (whose `typeof` now asks
# a proxy whether it is a function), plain function calls through `any`, and calls through a
# proxy (which the base cannot run: a proxy of a function was no function). Linux only.
#
#   bench/compile/node_misc_instr.sh BASE_TREE BASE_VELT HEAD_TREE HEAD_VELT [WORK]
#
# Each side reads its own std (<tree>/std) and runtime (next to its velt). Prints
# `<row> base=<n> head=<n> <change>`.
set -uo pipefail
base=$(cd "$1" && pwd) bv=$2 head=$(cd "$3" && pwd) hv=$4
work=${5:-$(mktemp -d)}
mkdir -p "$work"
here=$(cd "$(dirname "$0")" && pwd)
printf 'console.log("hello");\n' > "$work/hello.velt"
python3 "$here/scale/app.py" 10000 "$work/app10k" > /dev/null
cp "$here/node_misc/"*.velt "$work/"
instr() { # <velt> <tree> <dir> <args...>: velt's own instructions
  local v=$1 t=$2 d=$3
  shift 3
  (cd "$d" && VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_STD="$t/std" valgrind --tool=cachegrind \
    --cache-sim=no --cachegrind-out-file=/dev/null "$v" "$@" 2>&1 >/dev/null |
    sed -n 's/.*I *refs: *//p' | tr -d ,)
}
run() { # <exe>: the program's instructions, or "fails" when it does not end well
  local out
  out=$(valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$1" 2>&1 >/dev/null)
  echo "$out" | grep -q 'I *refs' && ! "$1" > /dev/null 2>&1 && { echo fails; return; }
  echo "$out" | sed -n 's/.*I *refs: *//p' | tr -d ,
}
row() { # <name> <base n> <head n>
  if [[ $2 =~ ^[0-9]+$ && $3 =~ ^[0-9]+$ ]]; then
    awk -v n="$1" -v b="$2" -v h="$3" 'BEGIN { printf "%s base=%d head=%d %+.2f%%\n", n, b, h, (h - b) * 100 / b }'
  else
    echo "$1 base=$2 head=$3"
  fi
}
row "hello vir" "$(instr "$bv" "$base" "$work" build --emit=vir hello.velt)" \
  "$(instr "$hv" "$head" "$work" build --emit=vir hello.velt)"
row "app10k vir" "$(instr "$bv" "$base" "$work/app10k" build --emit=vir main.velt)" \
  "$(instr "$hv" "$head" "$work/app10k" build --emit=vir main.velt)"
for p in url_domains_loop url_parse_loop proxy_get_typeof_loop any_call_loop proxy_call_loop; do
  for side in base head; do
    if [ $side = base ]; then v=$bv t=$base; else v=$hv t=$head; fi
    (cd "$work" && VELT_STD="$t/std" "$v" build --release "$p.velt" -o "$p.$side" > /dev/null 2>&1)
  done
  b=$( [ -x "$work/$p.base" ] && run "$work/$p.base" || echo "no build")
  h=$( [ -x "$work/$p.head" ] && run "$work/$p.head" || echo "no build")
  row "$p run" "$b" "$h"
done
