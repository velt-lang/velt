#!/usr/bin/env python3
"""The build-speed yardstick (#2707): what `velt check`, a debug build and a release build of
the generated application (app.py) cost per stage, and whether that cost grows linearly with
the program.

    yardstick.py [--velt PATH] [--sizes 10000,100000] [--modes check,debug,release]
                 [--release-sizes 10000] [--children] [--budgets FILE] [--check]
                 [--write-budgets] [--out DIR] [--md FILE]

For each size it generates the application (app.py) and, for each mode, runs velt:
- natively, for the wall and CPU seconds and the peak resident set of the largest process;
- under callgrind (velt only: the clang, cc and ld processes run natively), which dumps its
  counters at the end of every stage (`Session::record_time`, which `-v` prints), so each dump
  holds the instructions retired in one stage (`optimize` is split further at the end of
  `numrep` and of each VIR verification);
- for debug builds, with `--emit vir` and `VELT_LOWER_PROFILE=1`, for the functions and
  statements lowered (summed over the lowering passes) and the VIR lines; for release builds,
  with `--emit llvm` for the LLVM IR lines.
With `--children`, `clang`, `cc` and `ld.lld` are found first on PATH as wrappers that run them
under cachegrind, and their instructions are the stages `clang` and `linker` (slow: release
clang under valgrind takes a long time; leave it to on-demand runs).

Every run has a fresh empty VELT_HOME and VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off, and runs
on at most 4 cores (`taskset`), so the counts and the peak don't depend on the machine's size.

`--check` compares with the budgets file (budgets.tsv next to this script):
- linear cost: between two sizes, every stage's instructions, the instructions in all, the
  functions and statements lowered and the peak memory may grow at most as fast as 2.1x per
  doubling of the lines, (n2/n1)^log2(2.1) (11.8x for ten times the lines); stages under 1% of
  the larger run's instructions are not compared;
- budgets: instructions per line per stage and in all, and the peak memory, may exceed the
  recorded values by at most the file's tolerance.
A line `superlinear <mode> <metric> <ratio>` in the budgets file records a growth known today,
which may not get worse; fixing it removes the line (the list only shrinks). Exits 1 on a
failure, with the failing rows marked **over** in the table. `--write-budgets` writes the file
from this run.

Linux only (callgrind and taskset). Needs python3 and valgrind.
"""
import argparse
import concurrent.futures
import glob
import math
import os
import queue
import re
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", "..", ".."))
# Doubling the lines may cost at most this many times as much.
DOUBLING = 2.1
# Stages under this share of a run's instructions are not compared between sizes.
MIN_SHARE = 0.01
# Each run gets 4 cores: `--jobs N` runs N measurements at once on cores 0-3, 4-7, ...
CORES_PER_JOB = 4
# Where callgrind dumps its counters: the end of each stage.
DUMP_AFTER = "*Session>::record_time"
# The processes callgrind does not follow (they are counted by --children, or not at all): every
# child but velt itself (the source generators' host).
SKIP_CHILDREN = "*clang*,*cc,*gcc*,*ld,*ld.*,*lld*,*collect2*,*mold*"
# Parts of `optimize`, by the inclusive instructions of their entry functions there.
OPTIMIZE_PARTS = [("numrep", "velt_opt::numrep::run"),
                  ("VIR verify", "velt_vir::verify::verify_program_on::{closure#1}")]


def allowed_ratio(n1, n2):
    return (n2 / n1) ** math.log2(DOUBLING)


def env_for(home):
    env = dict(os.environ)
    env.update(VELT_HOME=home, VELT_BUILD_CACHE="off", VELT_BUILD_STAMP="off")
    env.setdefault("VELT_STD", os.path.join(ROOT, "std"))
    env.pop("VELT_LOWER_PROFILE", None)
    return env


def pinned(cmd, cores):
    return (["taskset", "-c", cores] if shutil.which("taskset") else []) + cmd


def run(cmd, cwd, env, err_path, out_path=os.devnull):
    """Run `cmd`; its exit status, wall and CPU seconds and peak resident set in MB."""
    t0 = time.perf_counter()
    with open(err_path, "w") as err, open(out_path, "w") as out:
        p = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=out, stderr=err)
        _, status, ru = os.wait4(p.pid, 0)
    wall = time.perf_counter() - t0
    rc = os.waitstatus_to_exitcode(status)
    return rc, wall, ru.ru_utime + ru.ru_stime, ru.ru_maxrss / 1024


def velt_cmd(velt, mode, out_file, extra=()):
    if mode == "check":
        return [velt, "check", "-v", "main.velt", *extra]
    rel = ["--release"] if mode == "release" else []
    return [velt, "build", *rel, "-v", "main.velt", "-o", out_file, *extra]


def stage_names(err_path):
    """The stages in the order `-v` printed them (`velt: <name> <ms> ms`)."""
    names = []
    for line in open(err_path, errors="replace"):
        m = re.match(r"velt: (\S+(?: \S+)?)\s+[\d.]+ ms", line)
        if m and m.group(1) != "total":
            names.append(m.group(1))
    return names


def callgrind_parts(prefix):
    """The dumps of the main process, in order ([(path, instructions)]), and the instructions
    of its children (the source generators' host)."""
    by_pid, hosts = {}, set()
    for path in glob.glob(f"{prefix}.*"):
        rest = path[len(prefix) + 1:].split(".")
        pid, part, total = int(rest[0]), 0, None
        with open(path, errors="replace") as f:
            for line in f:
                if line.startswith("cmd:") and "__generate-host" in line:
                    hosts.add(pid)
                elif line.startswith("part:"):
                    part = int(line.split()[1])
                elif line.startswith(("summary:", "totals:")):
                    total = int(line.split()[1])
                    break
        if total is not None:
            by_pid.setdefault(pid, []).append((part, path, total))
    main = min(p for p in by_pid if p not in hosts)
    parts = [(path, n) for _, path, n in sorted(by_pid.pop(main))]
    children = sum(n for ps in by_pid.values() for _, _, n in ps)
    return parts, children


def inclusive(path, functions):
    """The inclusive instructions of `functions` in one callgrind dump."""
    out = subprocess.run(["callgrind_annotate", "--inclusive=yes", "--threshold=100", path],
                         capture_output=True, text=True).stdout
    found = dict.fromkeys(functions, 0)
    for line in out.splitlines():
        m = re.match(r"\s*([\d,]+) \(.*?\)\s+\S*?:(.*?) \[", line)
        if m and m.group(2) in found:
            found[m.group(2)] = max(found[m.group(2)], int(m.group(1).replace(",", "")))
    return found


def stages_of(parts, names, children):
    """Each stage's instructions: dump i ended stage `names[i]` (`-v`'s order); the last dump
    (the exit) is `exit`. `optimize` is split into its OPTIMIZE_PARTS and the rest."""
    stages = {}
    for i, (path, n) in enumerate(parts):
        name = names[i] if i < len(names) else "exit"
        if name == "optimize":
            inc = inclusive(path, [f for _, f in OPTIMIZE_PARTS])
            for label, f in OPTIMIZE_PARTS:
                stages[f"optimize: {label}"] = stages.get(f"optimize: {label}", 0) + inc[f]
                n -= inc[f]
            name = "optimize: rest"
        stages[name] = stages.get(name, 0) + n
    if children:
        stages["generator host"] = children
    return stages


def child_wrappers(work):
    """A directory of `clang`, `cc` and `ld.lld` wrappers that run the real tool under
    cachegrind, writing to `<work>/children`."""
    bindir, outdir = os.path.join(work, "wrap"), os.path.join(work, "children")
    os.makedirs(bindir, exist_ok=True)
    os.makedirs(outdir, exist_ok=True)
    for tool in ["clang", "cc", "ld.lld", "ld"]:
        real = shutil.which(tool)
        if not real:
            continue
        kind = "clang" if tool == "clang" else "linker"
        with open(os.path.join(bindir, tool), "w") as f:
            f.write("#!/bin/sh\n"
                    f'exec valgrind --tool=cachegrind --cache-sim=no --trace-children=yes '
                    f'--cachegrind-out-file="$YARDSTICK_CHILDREN/{kind}.%p" -q {real} "$@"\n')
        os.chmod(os.path.join(bindir, tool), 0o755)
    return bindir, outdir


def children_totals(outdir):
    totals = {}
    for path in glob.glob(os.path.join(outdir, "*.*")):
        kind = os.path.basename(path).split(".")[0]
        for line in open(path, errors="replace"):
            if line.startswith("summary:"):
                totals[kind] = totals.get(kind, 0) + int(line.split()[1])
                break
    return totals


def lower_counts(err_path):
    """Functions and statements lowered (summed over the passes) and the number of passes."""
    fns = stmts = passes = 0
    pat = re.compile(r"velt-lower: pass \d+: [\d.]+ ms, (\d+) functions, \d+ blocks, (\d+) statements")
    for line in open(err_path, errors="replace"):
        m = pat.match(line)
        if m:
            fns += int(m.group(1))
            stmts += int(m.group(2))
            passes += 1
    return fns, stmts, passes


def count_lines(paths):
    n = 0
    for p in paths:
        with open(p, "rb") as f:
            n += sum(1 for _ in f)
    return n


def measure(velt, mode, size, src, lines, work, children, cores):
    os.makedirs(work, exist_ok=True)
    res = {"mode": mode, "size": size, "lines": lines, "stages": {}}

    def home():
        return tempfile.mkdtemp(prefix="home.", dir=work)

    def pinned_cmd(cmd):
        return pinned(cmd, cores)

    def fail(what, err):
        tail = "".join(open(err, errors="replace").readlines()[-30:])
        sys.exit(f"{mode} at {size} lines: {what} failed\n{tail}")

    # Natively: time and peak memory.
    err = os.path.join(work, "native.err")
    rc, wall, cpu, rss = run(pinned_cmd(velt_cmd(velt, mode, os.path.join(work, "app"))), src,
                             env_for(home()), err)
    if rc != 0:
        fail("the native run", err)
    res.update(wall=wall, cpu=cpu, peak_mb=rss)
    names = stage_names(err)

    # The counts of the code.
    if mode == "debug":
        env = env_for(home())
        env["VELT_LOWER_PROFILE"] = "1"
        err = os.path.join(work, "vir.err")
        # The text goes to stdout.
        vir = os.path.join(work, "app.vir")
        rc, *_ = run(pinned_cmd(velt_cmd(velt, mode, os.path.join(work, "app-vir"), ["--emit", "vir"])),
                     src, env, err, vir)
        if rc != 0:
            fail("--emit vir", err)
        res["functions"], res["statements"], res["passes"] = lower_counts(err)
        res["ir_lines"] = count_lines([vir])
    elif mode == "release":
        err = os.path.join(work, "llvm.err")
        ll = os.path.join(work, "app.ll")
        rc, *_ = run(pinned_cmd(velt_cmd(velt, mode, os.path.join(work, "app-ll"), ["--emit", "llvm"])),
                     src, env_for(home()), err, ll)
        if rc != 0:
            fail("--emit llvm", err)
        res["ir_lines"] = count_lines([ll])

    # Instructions per stage.
    env = env_for(home())
    # The source generators' host runs under valgrind too, far slower than its time limit expects.
    env["VELT_GENERATOR_TIMEOUT"] = "36000"
    if children:
        bindir, outdir = child_wrappers(work)
        env["PATH"] = bindir + os.pathsep + env["PATH"]
        env["YARDSTICK_CHILDREN"] = outdir
    prefix = os.path.join(work, "cg")
    for old in glob.glob(prefix + ".*"):
        os.remove(old)
    cg = ["valgrind", "--tool=callgrind", f"--callgrind-out-file={prefix}.%p",
          f"--dump-after={DUMP_AFTER}", "--trace-children=yes",
          f"--trace-children-skip={SKIP_CHILDREN}"]
    err = os.path.join(work, "callgrind.err")
    rc, *_ = run(pinned_cmd(cg + velt_cmd(velt, mode, os.path.join(work, "app-cg"))), src, env, err)
    if rc != 0:
        fail("the callgrind run", err)
    parts, host = callgrind_parts(prefix)
    names = stage_names(err)
    if len(parts) != len(names) + 1:
        sys.exit(f"{mode} at {size} lines: {len(parts)} callgrind dumps for {len(names)} stages "
                 f"({', '.join(names)}): is `Session::record_time` inlined now?")
    res["stages"] = stages_of(parts, names, host)
    if children:
        for kind, n in children_totals(outdir).items():
            res["stages"][kind] = n
    res["instructions"] = sum(res["stages"].values())
    return res


# --- budgets -------------------------------------------------------------------------------

def read_budgets(path):
    tol, budgets, superlinear = 0.10, {}, {}
    if not os.path.exists(path):
        return tol, budgets, superlinear
    for line in open(path):
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        f = line.split("\t")
        if f[0] == "tolerance":
            tol = float(f[1])
        elif f[0] == "budget":
            # budget <mode> <size> <metric> <value>
            budgets[(f[1], int(f[2]), f[3])] = float(f[4])
        elif f[0] == "superlinear":
            # superlinear <mode> <metric> <ratio per doubling>
            superlinear[(f[1], f[2])] = float(f[3])
    return tol, budgets, superlinear


def metrics(r):
    """The checked metrics of a run: name -> (value, value per line or None)."""
    m = {}
    for stage, n in r["stages"].items():
        m[f"instr:{stage}"] = n
    m["instr:all"] = r["instructions"]
    m["peak_mb"] = r["peak_mb"]
    for k in ("functions", "statements"):
        if k in r:
            m[k] = r[k]
    return m


def per_doubling(v1, v2, n1, n2):
    return (v2 / v1) ** (1 / math.log2(n2 / n1)) if v1 > 0 else float("inf")


def check(results, budgets_path):
    tol, budgets, superlinear = read_budgets(budgets_path)
    failures = []
    by_mode = {}
    for r in results:
        by_mode.setdefault(r["mode"], []).append(r)
    growth_rows = []
    for mode, rs in by_mode.items():
        rs.sort(key=lambda r: r["lines"])
        for a, b in zip(rs, rs[1:]):
            ma, mb = metrics(a), metrics(b)
            for key, v2 in mb.items():
                v1 = ma.get(key)
                if v1 is None:
                    continue
                if key.startswith("instr:") and key != "instr:all" and v2 < MIN_SHARE * b["instructions"]:
                    continue
                d = per_doubling(v1, v2, a["lines"], b["lines"])
                limit = max(DOUBLING, superlinear.get((mode, key), 0))
                over = d > limit + 1e-9
                known = (mode, key) in superlinear
                growth_rows.append((mode, key, a["size"], b["size"], v1, v2, d, limit, over, known))
                if over:
                    failures.append(f"{mode} {key}: {d:.2f}x per doubling, limit {limit:.2f}x")
    budget_rows = []
    for r in results:
        for key, v in metrics(r).items():
            if key in ("functions", "statements"):
                continue
            per = v / r["lines"] if key.startswith("instr:") else v
            b = budgets.get((r["mode"], r["size"], key))
            if b is None:
                continue
            over = per > b * (1 + tol)
            budget_rows.append((r["mode"], r["size"], key, per, b, over))
            if over:
                failures.append(f"{r['mode']} {r['size']} {key}: {per:.0f} over budget {b:.0f} (+{tol:.0%})")
    return growth_rows, budget_rows, failures, superlinear


def write_budgets(results, path, growth_rows, superlinear_old):
    out = [
        "# Budgets of the build-speed yardstick (bench/compile/scale/yardstick.py, #2707).",
        "# Written by `yardstick.py --write-budgets` on Linux x86-64; edit only to tighten.",
        "# budget <mode> <size> <metric> <value>: instructions per source line (instr:<stage>,",
        "# instr:all) or peak resident set in MB (peak_mb), at most `tolerance` over.",
        "# superlinear <mode> <metric> <growth per doubling>: a stage that grows faster than",
        "# 2.1x per doubling today; it may not get worse, and the line goes once it is fixed.",
        "tolerance\t0.10",
    ]
    for (mode, key, _, _, _, _, d, _, over, _) in growth_rows:
        if over or (mode, key) in superlinear_old:
            if d > DOUBLING:
                out.append(f"superlinear\t{mode}\t{key}\t{d:.2f}")
    for r in results:
        for key, v in metrics(r).items():
            if key in ("functions", "statements"):
                continue
            if key.startswith("instr:") and key != "instr:all" and v < MIN_SHARE * r["instructions"]:
                continue
            per = v / r["lines"] if key.startswith("instr:") else v
            out.append(f"budget\t{r['mode']}\t{r['size']}\t{key}\t{per:.0f}")
    with open(path, "w", newline="\n") as f:
        f.write("\n".join(out) + "\n")


# --- the report ----------------------------------------------------------------------------

def report(results, growth_rows, budget_rows):
    out = []
    stages = []
    for r in results:
        for s in r["stages"]:
            if s not in stages and any(x["stages"].get(s) for x in results):
                stages.append(s)
    # The generators' host runs inside sema; the exit is last.
    for name, after in [("generator host", "sema"), ("exit", None)]:
        if name in stages:
            stages.remove(name)
            stages.insert(stages.index(after) + 1 if after in stages else len(stages), name)
    out.append("#### Instructions per stage (millions) and per source line")
    out.append("")
    head = "| stage | " + " | ".join(f"{r['mode']} {r['size'] // 1000}k" for r in results) + " |"
    out += [head, "|---|" + "---:|" * len(results)]
    for s in stages + ["all"]:
        cells = []
        for r in results:
            n = r["instructions"] if s == "all" else r["stages"].get(s)
            cells.append("" if n is None else f"{n / 1e6:,.0f} ({n / r['lines']:,.0f})")
        name = f"**{s}**" if s == "all" else s
        out.append(f"| {name} | " + " | ".join(cells) + " |")
    out.append("")
    out.append("#### The code and the process")
    out.append("")
    out += [head.replace("| stage |", "| |"), "|---|" + "---:|" * len(results)]
    for key, fmt in [("lines", "{:,}"), ("functions", "{:,}"), ("statements", "{:,}"),
                     ("passes", "{}"), ("ir_lines", "{:,}"), ("peak_mb", "{:,.0f}"),
                     ("wall", "{:.1f}"), ("cpu", "{:.1f}")]:
        label = {"functions": "functions lowered", "statements": "statements lowered",
                 "passes": "lowering passes", "ir_lines": "IR lines (VIR debug, LLVM release)",
                 "peak_mb": "peak memory (MB)", "wall": "wall s (4 cores)",
                 "cpu": "CPU s"}.get(key, key)
        cells = [fmt.format(r[key]) if key in r else "" for r in results]
        out.append(f"| {label} | " + " | ".join(cells) + " |")
    if growth_rows:
        out.append("")
        out.append(f"#### Growth per doubling of the lines (limit {DOUBLING}x)")
        out.append("")
        out += ["| mode | metric | lines | growth | limit | |", "|---|---|---|---:|---:|---|"]
        for (mode, key, n1, n2, v1, v2, d, limit, over, known) in growth_rows:
            mark = "**over**" if over else ("known superlinear" if known else "")
            out.append(f"| {mode} | {key} | {n1 // 1000}k to {n2 // 1000}k | {d:.2f}x | {limit:.2f}x | {mark} |")
    bad = [b for b in budget_rows if b[5]]
    if budget_rows:
        out.append("")
        out.append(f"Budgets: {len(budget_rows) - len(bad)} of {len(budget_rows)} within.")
        for (mode, n, key, per, b, _) in bad:
            out.append(f"- **over** {mode} {n // 1000}k {key}: {per:,.0f}, budget {b:,.0f}")
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--velt")
    ap.add_argument("--sizes", default="10000,100000")
    ap.add_argument("--modes", default="check,debug,release")
    ap.add_argument("--release-sizes", default=None,
                    help="sizes for release builds (default: --sizes)")
    ap.add_argument("--children", action="store_true")
    ap.add_argument("--jobs", type=int, default=1,
                    help="measurements at once, each on its own 4 cores")
    ap.add_argument("--budgets", default=os.path.join(HERE, "budgets.tsv"))
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--write-budgets", action="store_true")
    ap.add_argument("--out")
    ap.add_argument("--md")
    a = ap.parse_args()
    target = os.environ.get("CARGO_TARGET_DIR", os.path.join(ROOT, "target"))
    out = os.path.abspath(a.out or os.path.join(target, "bench", "compile", "yardstick"))
    velt = a.velt
    if not velt:
        print("building velt and the runtimes (release)...", file=sys.stderr, flush=True)
        subprocess.check_call(["cargo", "build", "--release", "-q", "-p", "veltc", "-p", "velt_rt",
                               "-p", "velt_rt_shared", "--manifest-path",
                               os.path.join(ROOT, "Cargo.toml")])
        velt = os.path.join(target, "release", "velt")
    velt = os.path.abspath(velt)
    sizes = [int(s) for s in a.sizes.split(",")]
    rel_sizes = [int(s) for s in (a.release_sizes or a.sizes).split(",")]
    modes = a.modes.split(",")
    tasks = []
    for n in sizes:
        src = os.path.join(out, f"app-{n}", "src")
        if os.path.exists(src):
            shutil.rmtree(src)
        made = subprocess.check_output([sys.executable, os.path.join(HERE, "app.py"), str(n), src],
                                       text=True)
        lines = int(made.split()[-2])
        for mode in modes:
            if mode != "release" or n in rel_sizes:
                tasks.append((mode, n, src, lines))
    # The longest first, so the runs at once end together.
    tasks.sort(key=lambda t: (-t[1], -modes.index(t[0])))
    cores = queue.Queue()
    for j in range(a.jobs):
        cores.put(f"{j * CORES_PER_JOB}-{j * CORES_PER_JOB + CORES_PER_JOB - 1}")

    def one(task):
        mode, n, src, lines = task
        c = cores.get()
        try:
            print(f"{mode} at {n} lines ({lines})...", file=sys.stderr, flush=True)
            return measure(velt, mode, n, src, lines, os.path.join(out, f"app-{n}", mode),
                           a.children, c)
        finally:
            cores.put(c)

    with concurrent.futures.ThreadPoolExecutor(max_workers=a.jobs) as pool:
        results = list(pool.map(one, tasks))
    results.sort(key=lambda r: (modes.index(r["mode"]), r["size"]))
    growth_rows, budget_rows, failures, superlinear = check(results, a.budgets)
    text = report(results, growth_rows, budget_rows if a.check else [])
    print(text)
    if a.md:
        with open(a.md, "w") as f:
            f.write(text)
    if a.write_budgets:
        write_budgets(results, a.budgets, growth_rows, superlinear)
        print(f"wrote {a.budgets}", file=sys.stderr)
    if a.check and failures:
        print("yardstick: " + "; ".join(failures), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
