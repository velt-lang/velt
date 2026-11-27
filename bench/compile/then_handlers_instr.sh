#!/usr/bin/env bash
# Instructions (cachegrind, one thread) two compilers execute checking, and lowering to VIR
# (`build --emit=vir`: no backend, no linker), hello, a generated 10,000-line application
# (scale/app.py) and programs of many `then`/`catch` handlers casting their reasons (#3924).
# Linux only (valgrind).
#
#   bench/compile/then_handlers_instr.sh BASE_TREE HEAD_TREE [WORK]
#
# Each tree is a checkout with its release compiler built (`cargo build --release -p veltc
# --bin velt`, in <tree>/target); each side reads its own std. Prints
# `<program> <check|vir> base=<n> head=<n> <change>`. The programs: `thens`, 400 functions of
# three handlers each, half on promises that never reject (their reasons are `never`), and
# `thens_rej`, all on promises that reject. A compiler without the #3924 fix can check `thens`
# but not lower it, so it has no `vir` row.
set -uo pipefail
base=$(cd "$1" && pwd) head=$(cd "$2" && pwd)
work=${3:-$(mktemp -d)}
mkdir -p "$work"
here=$(cd "$(dirname "$0")" && pwd)
printf 'console.log("hello");\n' > "$work/hello.velt"
python3 "$here/scale/app.py" 10000 "$work/app10k" > /dev/null
gen() { # <out> <every promise rejects: 1|0>
  python3 - "$1" "$2" <<'EOF'
import sys
out, all_reject = sys.argv[1], sys.argv[2] == "1"
n = 400
src = ["class HttpError extends Error {\n  status: number;\n  constructor(s: number, m: string) {\n"
       "    super(m);\n    this.status = s;\n  }\n}\n",
       "function later(n: number): Promise<number> {\n  return new Promise((r) => setTimeout(() => r(n), 0));\n}\n",
       "function failing(n: number): Promise<number> {\n"
       "  return new Promise((r, j) => setTimeout(() => j(new HttpError(n, \"bad\")), 0));\n}\n"]
for i in range(n):
    p = "failing" if all_reject or i % 2 else "later"
    src.append(f"""async function h{i}(): Promise<number> {{
  let t = 0;
  await {p}({i}).then(
    (v) => {{ t += v; }},
    (e) => {{ const x = e as Error; t += x.message.length; }},
  );
  const c = await {p}({i}).catch((e) => (e as Error).message.length);
  await {p}({i}).then((v) => {{ t += v; }}).catch((e) => {{ console.log("e", (e as Error).message); }});
  return t + c;
}}
""")
src.append("async function main(): Promise<void> {\n  let s = 0;\n")
src += [f"  s += await h{i}();\n" for i in range(n)]
src.append("  console.log(s);\n}\nmain();\n")
open(out, "w").write("".join(src))
EOF
}
gen "$work/thens.velt" 0
gen "$work/thens_rej.velt" 1
ir() { # <tree> <dir> <check|vir> <file>
  local cmd=(check "$4")
  [ "$3" = vir ] && cmd=(build --emit=vir "$4")
  (cd "$2" && VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_STD="$1/std" valgrind --tool=cachegrind \
    --cache-sim=no --cachegrind-out-file=/dev/null "$1/target/release/velt" "${cmd[@]}" 2>&1 >/dev/null |
    sed -n 's/.*I *refs: *//p' | tr -d ,)
}
for spec in hello::hello.velt:check hello::hello.velt:vir app10k:app10k:main.velt:check \
            app10k:app10k:main.velt:vir thens::thens.velt:check thens_rej::thens_rej.velt:check \
            thens_rej::thens_rej.velt:vir; do
  IFS=: read -r name dir file mode <<< "$spec"
  b=$(ir "$base" "$work/$dir" "$mode" "$file") h=$(ir "$head" "$work/$dir" "$mode" "$file")
  awk -v n="$name" -v m="$mode" -v b="$b" -v h="$h" \
    'BEGIN { printf "%-10s %-6s base=%s head=%s %+.3f%%\n", n, m, b, h, 100 * (h - b) / b }'
done
