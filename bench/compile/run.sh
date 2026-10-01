#!/usr/bin/env bash
# Compile-time benchmark (Linux / macOS): same as run.ps1. `velt build --release --emit vir`
# (load + parse, sema, lowering to VIR, verification, the optimizer) on
# examples/http_hello.vlt, all_std.vlt, two programs of UNITS generated units (unit.tmpl;
# "units" with call chains of 8, "chain" with one call chain through the whole program) and
# "long_main" (4 × UNITS classes with an override and a generic instance each, all used from one
# `main`), best of RUNS runs per stage as `velt build -v` reports it. A second table times
# `velt check` (parse + sema only, and the whole command) and the link of a debug build: against
# the shared runtime (the default), the static one (VELT_RT_LINK=static), and a rebuild with
# nothing changed (the link is skipped).
#
#   bench/compile/run.sh [runs] [units] [path to velt]
#
# Without a velt path it builds velt (release) first. Needs python3 (or python) on PATH.
set -euo pipefail
RUNS=${1:-10}
UNITS=${2:-1000}
VELT=${3:-}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
OUT="$TARGET/bench/compile"
mkdir -p "$OUT"
PYTHON=$(command -v python3 || command -v python)

if [ -z "$VELT" ]; then
  echo "building velt and the runtimes (release)..." >&2
  cargo build --release -q -p veltc -p velt_rt -p velt_rt_shared --manifest-path "$ROOT/Cargo.toml"
  VELT="$TARGET/release/velt"
fi

"$PYTHON" - "$RUNS" "$UNITS" "$VELT" "$HERE" "$ROOT" "$OUT" <<'EOF'
import os, re, subprocess, sys, time
runs, units, velt, here, root, out = int(sys.argv[1]), int(sys.argv[2]), *sys.argv[3:]

def program(n, chain):
    unit = open(f"{here}/unit.tmpl").read().replace("\r\n", "\n")
    parts = ["interface Scorer {\n  score(x: i64): i64;\n}\n\n"]
    for i in range(n):
        link = ("  xs.push(1);\n  const r = 0;" if i % chain == 0
                else f"  const r = g{i - 1}(xs, new Base{i - 1}(0));")
        parts.append(unit.replace("@CHAIN@", link).replace("@I@", str(i)))
    parts.append("function main() {\n  const xs: i64[] = [];\n")
    parts += [f"  f{i}(new Sub{i}({i}, 1.0), xs, \"s\");\n" for i in range(n)]
    return "".join(parts) + "}\n"

def long_main(n):
    parts = ["function count<T>(xs: T[]): i64 {\n  return xs.length as i64;\n}\n\n"]
    for i in range(n):
        parts.append(f"class C{i} {{\n  id: i64;\n  constructor(id: i64) {{\n    this.id = id;\n  }}\n"
                     f"  area(): i64 {{\n    return this.id;\n  }}\n}}\n\n")
        parts.append(f"class S{i} extends C{i} {{\n  constructor(id: i64) {{\n    super(id);\n  }}\n"
                     f"  override area(): i64 {{\n    return this.id + 1;\n  }}\n}}\n\n")
    parts.append("function main() {\n  let t = 0;\n")
    parts += [f"  const b{i}: C{i} = new S{i}({i});\n  t += b{i}.area() + count([b{i}]);\n"
              for i in range(n)]
    return "".join(parts) + "  console.log(t);\n}\n"

programs = {"http_hello": f"{root}/examples/http_hello.vlt", "all_std": f"{here}/all_std.vlt"}
for name, text in ((f"units_{units}", program(units, 8)), (f"chain_{units}", program(units, units)),
                   (f"long_main_{units * 4}", long_main(units * 4))):
    path = f"{out}/{name}.vlt"
    open(path, "w").write(text)
    programs[name] = path

stages = ["parse", "sema", "lower", "verify", "optimize"]
front = ["parse", "sema", "lower"]
print("| program | root file lines | load + parse | sema | lower | verify | optimize | front end |")
print("|---|---|---|---|---|---|---|---|")
for name, path in programs.items():
    best = {}
    for _ in range(runs):
        r = subprocess.run([velt, "build", "-v", "--release", path, "--emit", "vir"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        if r.returncode != 0:
            sys.exit(f"velt build {path} failed:\n{r.stderr}")
        times = dict((m[1], float(m[2])) for m in re.finditer(r"^velt: (\w+)\s+([\d.]+) ms", r.stderr, re.M))
        times["front"] = sum(times[s] for s in front)
        for k, v in times.items():
            best[k] = min(best.get(k, v), v)
    lines = sum(1 for _ in open(path))
    print(f"| {name} | {lines} | " + " | ".join(f"{best[s]:.1f}" for s in stages + ["front"]) + " |")
print()
print(f"Best of {runs} runs per stage, milliseconds (`velt build -v --release --emit vir`); "
      "front end = best run's parse + sema + lower.")

def timed(args, env=None):
    """Run velt; (stage times from -v, wall milliseconds)."""
    start = time.perf_counter()
    r = subprocess.run([velt, *args], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
                       env=env)
    wall = (time.perf_counter() - start) * 1e3
    if r.returncode != 0:
        sys.exit(f"velt {' '.join(args)} failed:\n{r.stderr}")
    return dict((m[1], float(m[2])) for m in re.finditer(r"^velt: (\w+)\s+([\d.]+) ms", r.stderr, re.M)), wall

def debug_link(path, exe, env, relink):
    """Link time of a debug build of path; relink=False deletes the link stamp first."""
    if not relink and os.path.exists(exe + ".link-stamp"):
        os.remove(exe + ".link-stamp")
    return timed(["build", "-v", path, "-o", exe], env)[0]["link"]

static_env = dict(os.environ, VELT_RT_LINK="static")
print()
print("| program | check: parse + sema | check: whole command | debug link: shared runtime | "
      "debug link: static runtime | rebuild, nothing changed: link |")
print("|---|---|---|---|---|---|")
for name in ["http_hello", "all_std", f"units_{units}"]:
    path, exe = programs[name], f"{out}/{name}_debug"
    check, wall = [], []
    for _ in range(runs):
        t, w = timed(["check", "-v", path])
        check.append(t["parse"] + t["sema"])
        wall.append(w)
    shared = min(debug_link(path, exe, None, False) for _ in range(runs))
    static = min(debug_link(path, exe, static_env, False) for _ in range(runs))
    relink = min(debug_link(path, exe, static_env, True) for _ in range(runs))
    print(f"| {name} | {min(check):.1f} | {min(wall):.1f} | {shared:.1f} | {static:.1f} | {relink:.1f} |")
print()
print(f"Best of {runs} runs, milliseconds: `velt check -v` (its parse + sema, and the wall time of "
      "the whole process) and the `link` stage of `velt build -v` (debug).")
EOF
