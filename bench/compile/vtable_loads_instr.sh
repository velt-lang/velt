#!/usr/bin/env bash
# Instructions the compiler itself executes (clang and the built program are child processes and
# are not counted) on the programs behind the vtable_loads cost of #2342, and the share of the
# functions of `vtable_loads`. Linux only (valgrind).
#
#   bench/compile/vtable_loads_instr.sh VELT STD_DIR ROOT OUT_DIR [NAME...]
#
# VELT is a built compiler with its runtime next to it, ROOT a checkout (the programs are read
# from it), OUT_DIR gets NAME.cg and NAME.annotate. Prints "NAME instructions pass-instructions".
# NAME: this_fn (a large golden, --release, as the harness builds it but with the build cache
# off), hello (check), all_std (check, every std definition), jsx (bench/jsx/main.velt,
# --release). The pass's instructions are the self cost of the functions and source lines under
# vtable_loads in the cachegrind output; helpers outside it, memcpy and the allocator are not.
set -uo pipefail
velt=$1 std=$2 root=$3 out=$4
shift 4
names=("$@")
[ ${#names[@]} -gt 0 ] || names=(hello all_std jsx this_fn)
mkdir -p "$out"
home=$(mktemp -d)
trap 'rm -rf "$home"' EXIT
run() {
  local name=$1
  shift
  (cd "$root" && env VELT_HOME="$home" VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_STD="$std" "$@" \
    valgrind --vgdb=no --tool=cachegrind --cache-sim=no --cachegrind-out-file="$out/$name.cg" \
    "$velt" "${CMD[@]}") </dev/null >"$out/$name.stdout" 2>"$out/$name.vg"
  cg_annotate --show-percs=no --threshold=0.01 "$out/$name.cg" >"$out/$name.annotate" 2>/dev/null
  local ir pass
  ir=$(grep -oP 'I\s+refs:\s+\K[0-9,]+' "$out/$name.vg" | tail -1 | tr -d ,)
  pass=$(awk '/^fl=/ { fl = $0 } /^fn=/ { fn = $0 }
    /^[0-9]/ { if (fn ~ /vtable_loads/ || fl ~ /vtable_loads/) s += $2 } END { printf "%d", s }' "$out/$name.cg")
  echo "$name $ir $pass"
}
for name in "${names[@]}"; do
  case $name in
    this_fn) CMD=(run --release tests/golden/lang/this_fn_array_prototype_sort.velt); run "$name" ;;
    hello) CMD=(check tests/golden/m1/hello.velt); run "$name" ;;
    all_std) CMD=(check bench/compile/all_std.velt); run "$name" VELT_EAGER_STD=1 ;;
    jsx) CMD=(build --release bench/jsx/main.velt -o "$out/jsx.exe"); run "$name" ;;
    *) echo "unknown program $name" >&2 ;;
  esac
done
