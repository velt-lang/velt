#!/bin/bash
# Instruction counts (cachegrind) of `velt check` on generated programs with many module
# constants that read a module-level `let` (#1383): N constants `const Ci = { v: count + i }`,
# N functions each read by one constant, and a call chain N deep down to a function that reads
# the `let`. The cost should grow linearly with N.
#
# usage: bench/compile/module_consts.sh <velt> <std dir> [<velt> <std dir> ...]
# Prints `<program> N=<n> <velt> <instructions>` lines. Needs valgrind and python3.
set -eu
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
gen() {
  python3 - "$1" "$2" "$3" <<'EOF'
import sys
kind, n, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]
L = ["let count = 0;"]
if kind == "consts":
    L += [f"const C{i} = {{ v: count + {i} }};" for i in range(n)]
    L.append(f"function main() {{ count = 1; console.log(C0.v, C{n-1}.v); }}")
elif kind == "fns":
    L += [f"function f{i}(): number {{ return count + {i}; }}" for i in range(n)]
    L += [f"const K{i} = f{i}();" for i in range(n)]
    L.append(f"function main() {{ count = 1; console.log(K0, K{n-1}); }}")
else:
    L.append("function g0(): number { return count; }")
    L += [f"function g{i}(): number {{ return g{i-1}(); }}" for i in range(1, n)]
    L.append(f"const K = g{n-1}();")
    L.append("function main() { count = 1; console.log(K); }")
open(out, "w").write("\n".join(L) + "\n")
EOF
}
for spec in consts:3000 consts:6000 consts:12000 fns:6000 chain:1500 chain:6000; do
  kind=${spec%:*}
  n=${spec#*:}
  gen "$kind" "$n" "$work/$kind$n.vlt"
  i=0
  args=("$@")
  while [ $i -lt ${#args[@]} ]; do
    velt=${args[$i]}
    std=${args[$((i + 1))]}
    VELT_STD=$std valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
      "$velt" check "$work/$kind$n.vlt" >"$work/log" 2>&1 || true
    echo "$kind N=$n $velt $(grep -E 'I\s+refs' "$work/log" | awk '{print $NF}')"
    i=$((i + 2))
  done
done
