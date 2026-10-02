#!/usr/bin/env bash
# Run-time cost of codegen units (Linux / macOS). Builds composites large enough to split: the
# units_1000 program of run.sh (unit.tmpl, called once from `main`, so it is compiled) plus one
# benchmark (bench/*.vlt, and the benchmarks-game programs at the reduced sizes of RESULTS.md
# "Codegen round"), each with two configurations of `velt build --release --backend llvm`:
# the baseline (default: one unit) and the candidate (default: VELT_CODEGEN_UNITS=4). Both must
# print the same output; then ROUNDS interleaved rounds alternate which runs first, and the table
# shows best and median CPU time (user + system, from wait4) and the change, flagging changes of
# the best beyond ±3 %.
#
#   bench/compile/split_runtime.sh [--runs N] [--units N|auto] [--base-units N|auto]
#                                  [--velt PATH] [--base-velt PATH] [program...]
#
#   --runs N        interleaved rounds (default 9)
#   --units N       candidate VELT_CODEGEN_UNITS (default 4; `auto` leaves it unset)
#   --base-units N  baseline VELT_CODEGEN_UNITS (default 1)
#   --velt PATH     candidate compiler (default: the release velt, built first)
#   --base-velt P   baseline compiler (default: the candidate's), e.g. an older build to compare
#                   two placements with --base-units 4
#   program...      only composites whose name contains one of these
#
# Needs python3 (or python) and clang. The machine should be quiet: on a loaded one, rerun and
# compare before trusting a change.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
OUT="$TARGET/bench/split_runtime"
RUNS=9
UNITS=4
BASE_UNITS=1
VELT=
BASE_VELT=
ONLY=()
while [[ $# -gt 0 ]]; do
  case $1 in
    --runs) RUNS=$2; shift ;;
    --units) UNITS=$2; shift ;;
    --base-units) BASE_UNITS=$2; shift ;;
    --velt) VELT=$2; shift ;;
    --base-velt) BASE_VELT=$2; shift ;;
    -h|--help) sed -n '2,23p' "$0"; exit 0 ;;
    *) ONLY+=("$1") ;;
  esac
  shift
done
mkdir -p "$OUT"
PYTHON=$(command -v python3 || command -v python)
if [ -z "$VELT" ]; then
  echo "building velt and the runtimes (release)..." >&2
  cargo build --release -q -p veltc -p velt_rt -p velt_rt_shared --manifest-path "$ROOT/Cargo.toml"
  VELT="$TARGET/release/velt"
fi
BASE_VELT=${BASE_VELT:-$VELT}

"$PYTHON" - "$RUNS" "$UNITS" "$BASE_UNITS" "$VELT" "$BASE_VELT" "$HERE" "$ROOT" "$OUT" \
  "${ONLY[@]+"${ONLY[@]}"}" <<'EOF'
import os, re, statistics, subprocess, sys, tempfile
runs, units, base_units, velt, base_velt, here, root, out = sys.argv[1:9]
runs, only = int(runs), sys.argv[9:]
game = f"{root}/bench/benchmarks-game"

def units_code(n=1000, chain=8):
    """The units_1000 program of run.sh with its `main` renamed `units_main` and its names
    suffixed (`Base_u7`, `UnitScorer`) so they cannot clash with the benchmark's."""
    unit = open(f"{here}/unit.tmpl").read().replace("\r\n", "\n").replace("Scorer", "UnitScorer")
    parts = ["interface UnitScorer {\n  score(x: i64): i64;\n}\n\n"]
    for i in range(n):
        link = ("  xs.push(1);\n  const r = 0;" if i % chain == 0
                else f"  const r = g_u{i - 1}(xs, new Base_u{i - 1}(0));")
        parts.append(unit.replace("@CHAIN@", link).replace("k: @I@", f"k: {i}")
                     .replace("@I@", f"_u{i}"))
    parts.append("function units_main() {\n  const xs: i64[] = [];\n")
    parts += [f"  f_u{i}(new Sub_u{i}({i}, 1.0), xs, \"s\");\n" for i in range(n)]
    return "".join(parts) + "}\n"

def composite(source):
    """The benchmark with its `main` renamed, the units code, and a `main` calling both."""
    text = open(source).read().replace("\r\n", "\n")
    text, found = re.subn(r"^(async )?function main\(\)", r"\1function bench_main()", text,
                          count=1, flags=re.M)
    if not found:
        sys.exit(f"{source}: no `function main()`")
    # Some benchmarks predate the prelude's `toFixed` and define their own: keep theirs.
    if re.search(r"^extend f64 \{\n  toFixed\(", text, re.M):
        text = text.replace("toFixed(", "benchToFixed(")
    call = "await bench_main()" if "async function bench_main" in text else "bench_main()"
    head = "async function" if call.startswith("await") else "function"
    return f"{text}\n{units_code()}\n{head} main() {{\n  units_main();\n  {call};\n}}\n"

def fasta(n):
    """Path of the fasta output for n (stdin of k-nucleotide, reverse-complement, regex-redux)."""
    path = f"{out}/fasta-{n}.txt"
    if not os.path.exists(path) or os.path.getsize(path) == 0:
        exe = f"{out}/fasta-gen"
        build(velt, None, f"{game}/fasta/main.vlt", exe)
        with open(path, "wb") as f:
            subprocess.run([exe, str(n)], stdout=f, check=True)
    return path

# name: (source, arguments, stdin size)
programs = {os.path.basename(p)[:-4]: (f"{root}/bench/{p}", [], None)
            for p in sorted(os.listdir(f"{root}/bench")) if p.endswith(".vlt")}
programs.update({
    "binary-trees": (f"{game}/binary-trees/main.vlt", ["18"], None),
    "binary-trees arena": (f"{game}/binary-trees/main_arena.vlt", ["18"], None),
    "fannkuch-redux": (f"{game}/fannkuch-redux/main.vlt", ["10"], None),
    "fasta": (f"{game}/fasta/main.vlt", ["5000000"], None),
    "k-nucleotide": (f"{game}/k-nucleotide/main.vlt", [], 2000000),
    "mandelbrot": (f"{game}/mandelbrot/main.vlt", ["4000"], None),
    "mandelbrot opt": (f"{game}/mandelbrot/main_opt.vlt", ["4000"], None),
    "n-body": (f"{game}/n-body/main.vlt", ["10000000"], None),
    "n-body opt": (f"{game}/n-body/main_opt.vlt", ["10000000"], None),
    "pidigits": (f"{game}/pidigits/main.vlt", ["3000"], None),
    "pidigits limbs": (f"{game}/pidigits/main_limbs.vlt", ["5000"], None),
    "regex-redux": (f"{game}/regex-redux/main.vlt", [], 500000),
    "reverse-complement": (f"{game}/reverse-complement/main.vlt", [], 2000000),
    "spectral-norm": (f"{game}/spectral-norm/main.vlt", ["3000"], None),
})
if only:
    programs = {k: v for k, v in programs.items() if any(o in k for o in only)}

def build(compiler, n_units, source, exe):
    """Release build; returns (codegen ms from -v, number of objects)."""
    env = dict(os.environ)
    env.pop("VELT_CODEGEN_UNITS", None)
    if n_units not in (None, "auto"):
        env["VELT_CODEGEN_UNITS"] = str(n_units)
    r = subprocess.run([compiler, "build", "--release", "--backend", "llvm", "-v", source,
                        "-o", exe], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                       text=True)
    if r.returncode != 0:
        sys.exit(f"velt build {source} failed:\n{r.stderr}")
    codegen = re.search(r"^velt: codegen\s+([\d.]+) ms", r.stderr, re.M)
    objects = 1 + sum(1 for i in range(1, 256) if os.path.exists(f"{exe}.cgu{i}.o"))
    return float(codegen[1]) if codegen else float("nan"), objects

def run(exe, args, stdin):
    """CPU seconds (user + system) and stdout of one run."""
    with open(stdin or os.devnull, "rb") as inp, tempfile.TemporaryFile() as o:
        p = subprocess.Popen([exe, *args], stdin=inp, stdout=o, stderr=subprocess.DEVNULL)
        _, status, ru = os.wait4(p.pid, 0)
        if status != 0:
            sys.exit(f"{exe}: exit status {status}")
        o.seek(0)
        return ru.ru_utime + ru.ru_stime, o.read()

print(f"baseline: {base_velt} units={base_units}; candidate: {velt} units={units}; "
      f"{runs} interleaved rounds", file=sys.stderr)
print("| program | objects | base best | cand best | change | base median | cand median "
      "| change | |")
print("|---|---|---:|---:|---:|---:|---:|---:|---|")
flagged = []
for name, (source, args, stdin_size) in programs.items():
    slug = name.replace(" ", "_")
    src = f"{out}/{slug}.vlt"
    open(src, "w").write(composite(source))
    stdin = fasta(stdin_size) if stdin_size else None
    exes = [f"{out}/{slug}_base", f"{out}/{slug}_cand"]
    built = [build(base_velt, base_units, src, exes[0]), build(velt, units, src, exes[1])]
    # Untimed first runs check the output (and absorb macOS's first-launch scan).
    if run(exes[0], args, stdin)[1] != run(exes[1], args, stdin)[1]:
        sys.exit(f"{name}: the two builds print different output")
    times = [[], []]
    for r in range(runs):
        for k in ((0, 1) if r % 2 == 0 else (1, 0)):
            times[k].append(run(exes[k], args, stdin)[0])
    best = [min(t) for t in times]
    med = [statistics.median(t) for t in times]
    change = best[1] / best[0] - 1
    flag = "**> ±3 %**" if abs(change) > 0.03 else ""
    if flag:
        flagged.append(name)
    print(f"| {name} | {built[0][1]} / {built[1][1]} | {best[0]:.3f} | {best[1]:.3f} | "
          f"{change:+.1%} | {med[0]:.3f} | {med[1]:.3f} | {med[1] / med[0] - 1:+.1%} | {flag} |",
          flush=True)
    print(f"{name}: codegen {built[0][0] / 1e3:.1f} s / {built[1][0] / 1e3:.1f} s", file=sys.stderr)
print()
print(f"CPU seconds, best and median of {runs} interleaved rounds; objects: baseline / candidate.")
if flagged:
    print(f"Beyond ±3 %: {', '.join(flagged)}")
EOF
