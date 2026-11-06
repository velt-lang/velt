#!/usr/bin/env bash
# Generated code and compile cost of velt_opt's `fits_pair`, by instructions executed: pair.velt's
# two kernels, as they are and with `i8[]` boxed, and `--emit vir` of one function with N pairs (the
# pass's counts are per function), from this tree's compiler and, for the first column, another one:
#
#   VELT_BASE=path/to/base/velt bench/simd/pair.sh
#
# Prints a Markdown table. Uses `/usr/bin/time -l` on macOS and cachegrind on Linux, so a busy
# machine does not matter. Set VELT_STD to this tree's std for a compiler built elsewhere.
# Needs: cargo, clang, python3; valgrind on Linux.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  python3 -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-simd"
mkdir -p "$OUT"
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"

python3 - "$HERE/pair.velt" "$OUT" "$TARGET/release/velt" "${VELT_BASE:-}" <<'PY'
import os, re, subprocess, sys
src, out, head, base = sys.argv[1:]
def instructions(argv):
    if sys.platform == "darwin":
        p = subprocess.run(["/usr/bin/time", "-l"] + argv, check=True, capture_output=True, text=True)
        return int(re.search(r"(\d+)\s+instructions retired", p.stderr).group(1)), p.stdout
    log = out + "/pair.cg.log"
    p = subprocess.run(["valgrind", "--tool=cachegrind", "--cache-sim=no",
                        "--cachegrind-out-file=/dev/null", f"--log-file={log}"] + argv,
                       check=True, capture_output=True, text=True)
    return int(re.search(r"I\s+refs:\s+([\d,]+)", open(log).read()).group(1).replace(",", "")), p.stdout
def boxed():
    path = out + "/pair_boxed.velt"
    text = open(src).read().replace("  const kernels:", "  console.log(new Held(a).xs.length + new Held(a).xs.length);\n  const kernels:")
    with open(path, "w") as f:
        f.write(text + "\nclass Held {\n  xs: i8[];\n\n  constructor(xs: i8[]) {\n    this.xs = xs;\n  }\n}\n")
    return path
def stress(n):
    path = f"{out}/pair_stress_{n}.velt"
    tests = "".join(f"  if (i8x16.fits(a, j + {i}) && i8x16.fits(b, j + {i})) {{ c += {i}; }}\n" for i in range(n))
    with open(path, "w") as f:
        f.write('import { i8x16 } from "velt/simd";\nimport { args } from "velt/process";\n'
                "function f(a: i8[], b: i8[], j: usize): usize {\n"
                f"  let c: usize = 0;\n{tests}  return c;\n}}\n"
                "function main() {\n  const a: i8[] = [];\n  const b: i8[] = [];\n"
                "  console.log(f(a, b, args().length));\n}\n")
    return path
compilers = [("base", base)] * bool(base) + [("head", head)]
rows = {}
for who, velt in compilers:
    for form, path in (("", src), (", boxed", boxed())):
        exe = f"{out}/pair-{who}{form[2:]}"
        subprocess.run([velt, "build", "--release", "--backend", "llvm", path, "-o", exe], check=True)
        for name, argv in (("two-array dot", [exe]), ("one-array dot", [exe, "one"])):
            # Best of 3: the first run also pays for loading the program.
            rows.setdefault(name + form, []).append(min(instructions(argv) for _ in range(3)))
    for n in (1000, 4000):
        cmd = [velt, "build", "--release", "--emit", "vir", stress(n)]
        rows.setdefault(f"`--emit vir`, {n} pairs", []).append((instructions(cmd)[0], ""))
for name, seen in rows.items():
    if len({s for _, s in seen}) != 1:
        sys.exit(f"{name}: the compilers' programs print different output")
print("| what | " + " | ".join(w for w, _ in compilers) + (" | change |" if base else " |"))
print("|---|" + "---:|" * (len(compilers) + bool(base)))
for name, seen in rows.items():
    cells = [f"{n:,}" for n, _ in seen]
    if base:
        cells.append(f"{100 * (seen[1][0] - seen[0][0]) / seen[0][0]:+.2f}%")
    print(f"| {name} | " + " | ".join(cells) + " |")
PY
