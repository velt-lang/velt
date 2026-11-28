#!/usr/bin/env bash
# Instructions (cachegrind, one thread) two compilers execute checking, and lowering to VIR
# (`build --emit=vir`: no backend, no linker), hello, a generated 10,000-line application
# (scale/app.py) and a program of 300 classes whose methods implement nullable interface
# results with fewer nothings (#3843). Linux only (valgrind).
#
#   bench/compile/covariant_nullable_instr.sh BASE_TREE HEAD_TREE [WORK]
#
# Each tree is a checkout with its release compiler built (`cargo build --release -p veltc
# --bin velt`, in <tree>/target); each side reads its own std. Prints
# `<program> <check|vir> base=<n> head=<n> <change>`. `impls` has the same nothings on both
# sides (what both compilers accept); `impls_wider` has the shapes only the #3843 fix accepts,
# so a base compiler without it has no row for it.
set -uo pipefail
base=$(cd "$1" && pwd) head=$(cd "$2" && pwd)
work=${3:-$(mktemp -d)}
mkdir -p "$work"
here=$(cd "$(dirname "$0")" && pwd)
printf 'console.log("hello");\n' > "$work/hello.velt"
python3 "$here/scale/app.py" 10000 "$work/app10k" > /dev/null
gen() { # <out> <wider: 1|0>
  python3 - "$1" "$2" <<'EOF'
import sys
out, wider = sys.argv[1], sys.argv[2] == "1"
n = 300
src = ["interface Fluent {\n  total(): number;\n}\n"]
for i in range(n):
    r1, r2 = ("D%d" % i, "D%d | null" % i) if wider else ("D%d | null" % i, "D%d | null | undefined" % i)
    src.append(f"""class D{i} implements Fluent {{
  constructor(public t: number) {{}}
  total(): number {{
    return this.t + {i};
  }}
}}
interface G{i} {{
  a(): Fluent | null;
  b(k: number): Fluent | null | undefined;
}}
class I{i} implements G{i} {{
  a(): {r1} {{
    return new D{i}(1);
  }}
  b(k: number): {r2} {{
    return k == 0 ? null : new D{i}(k);
  }}
}}
""")
src.append("function main() {\n  let s = 0;\n")
for i in range(n):
    src.append(f"  const g{i}: G{i} = new I{i}();\n  s += (g{i}.a()?.total() ?? 0) + (g{i}.b(2)?.total() ?? 0);\n")
src.append("  console.log(s);\n}\n")
open(out, "w").write("".join(src))
EOF
}
gen "$work/impls.velt" 0
gen "$work/impls_wider.velt" 1
ir() { # <tree> <dir> <check|vir> <file>
  local cmd=(check "$4")
  [ "$3" = vir ] && cmd=(build --emit=vir "$4")
  (cd "$2" && VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_STD="$1/std" valgrind --tool=cachegrind \
    --cache-sim=no --cachegrind-out-file=/dev/null "$1/target/release/velt" "${cmd[@]}" 2>&1 >/dev/null |
    sed -n 's/.*I *refs: *//p' | tr -d ,)
}
for spec in hello::hello.velt:check hello::hello.velt:vir app10k:app10k:main.velt:check \
            app10k:app10k:main.velt:vir impls::impls.velt:check impls::impls.velt:vir \
            impls_wider::impls_wider.velt:check impls_wider::impls_wider.velt:vir; do
  IFS=: read -r name dir file mode <<< "$spec"
  b=$(ir "$base" "$work/$dir" "$mode" "$file") h=$(ir "$head" "$work/$dir" "$mode" "$file")
  awk -v n="$name" -v m="$mode" -v b="$b" -v h="$h" \
    'BEGIN { printf "%-12s %-6s base=%s head=%s %+.3f%%\n", n, m, b, h, (b > 0 ? 100 * (h - b) / b : 0) }'
done
