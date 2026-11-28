#!/usr/bin/env bash
# Instructions (cachegrind, one thread) two compilers execute checking and lowering to VIR
# (`build --emit=vir`: no backend, no linker) hello, a generated 10,000-line application
# (scale/app.py) and `anys`, a program of many async and sync functions calling through `any`
# outside any `catch` (whose `TypeError` is one of their errors since #3955), and running
# `anys` built with each (release). Linux only (valgrind).
#
#   bench/compile/not_fn_instr.sh BASE_TREE HEAD_TREE [WORK]
#
# Each tree is a checkout with its release compiler and runtime built in <tree>/target (`cargo
# build --release -p veltc --bin velt`, then `cargo build --release -p velt_rt -p velt_rt_shared`:
# one command for both leaves out the runtime's library); each side reads its own std. Prints
# `<program> <check|vir|run> base=<n> head=<n> <change>`.
set -uo pipefail
base=$(cd "$1" && pwd) head=$(cd "$2" && pwd)
work=${3:-$(mktemp -d)}
mkdir -p "$work"
here=$(cd "$(dirname "$0")" && pwd)
printf 'console.log("hello");\n' > "$work/hello.velt"
python3 "$here/scale/app.py" 10000 "$work/app10k" > /dev/null
python3 - "$work/anys.velt" <<'EOF'
import sys
n = 300
src = ["const o: any = { a: 1, f: (x: number): number => x + 1, g: (s: string): string => s };\n"]
for i in range(n):
    src.append(f"""function s{i}(x: any): number {{
  return x.f({i}) + x.f(x.a);
}}
async function a{i}(x: any): Promise<number> {{
  const v = await Promise.resolve(x.f({i}));
  return v + s{i}(x) + x.g("k").length;
}}
""")
src.append("async function main(): Promise<void> {\n  let t = 0;\n  for (let r = 0; r < 20; r++) {\n")
src += [f"    t += await a{i}(o);\n" for i in range(n)]
src.append("  }\n  console.log(t);\n}\nmain();\n")
open(sys.argv[1], "w").write("".join(src))
EOF
ir() { # <tree> <dir> <check|vir|run> <file>
  local cmd=("$1/target/release/velt" check "$4")
  [ "$3" = vir ] && cmd=("$1/target/release/velt" build --emit=vir "$4")
  if [ "$3" = run ]; then
    (cd "$2" && VELT_STD="$1/std" VELT_RT_LIB="$1/target/release/libvelt_rt.a" \
      "$1/target/release/velt" build --release -o "$work/anys-bin" "$4" > /dev/null 2>&1) || return
    cmd=("$work/anys-bin")
  fi
  (cd "$2" && VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_STD="$1/std" valgrind --tool=cachegrind \
    --cache-sim=no --cachegrind-out-file=/dev/null "${cmd[@]}" 2>&1 >/dev/null |
    sed -n 's/.*I *refs: *//p' | tr -d ,)
}
for spec in hello::hello.velt:check hello::hello.velt:vir app10k:app10k:main.velt:check \
  app10k:app10k:main.velt:vir anys::anys.velt:check anys::anys.velt:vir anys::anys.velt:run; do
  IFS=: read -r name sub file what <<< "$spec"
  dir=$work/$sub
  b=$(ir "$base" "$dir" "$what" "$file")
  h=$(ir "$head" "$dir" "$what" "$file")
  if [ -z "$b" ] || [ -z "$h" ]; then
    echo "$name $what base=${b:-none} head=${h:-none}"
    continue
  fi
  awk -v n="$name" -v w="$what" -v b="$b" -v h="$h" \
    'BEGIN { printf "%s %s base=%d head=%d %+.2f%%\n", n, w, b, h, (h - b) * 100 / b }'
done
