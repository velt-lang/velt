#!/usr/bin/env python3
"""The build-speed yardstick (#2707): what `velt check`, a debug build and a release build of
the generated application (app.py) cost per stage, and whether that cost grows linearly with
the program.

    yardstick.py [--velt PATH] [--sizes 10000,100000] [--modes check,debug,release]
                 [--release-sizes 10000] [--children] [--jobs N] [--budgets FILE] [--check]
                 [--write-budgets] [--out DIR] [--reuse] [--md FILE]

For each size it generates the application (app.py) and, for each mode, runs velt:
- natively three times, for the wall and CPU seconds and the peak resident set of the largest
  process (velt; the source generators' host and the linker are smaller), each the median;
- under callgrind, which dumps its counters at the end of every stage (`Session::record_time`,
  the stages `-v` prints), so each dump holds the instructions retired in one stage. `optimize`
  is split into numrep, its VIR verification and the rest by the inclusive counts of their
  entry functions. Callgrind follows the source generators' host (`velt __generate-host`, the
  stage `generator host`, which runs inside sema; any other velt child would be `other
  processes`) but not clang, cc or ld;
- for debug builds, with `--emit vir` and `VELT_LOWER_PROFILE=1`, for the functions and
  statements lowered (summed over the lowering passes, and in the last pass) and the VIR lines;
  for release builds, with `--emit llvm`, for the LLVM IR lines. Release builds at a size not in
  `--release-sizes` count only the LLVM IR lines.
With `--children`, `clang`, `cc`, `ld` and `ld.lld` are found first on PATH (clang also as
$VELT_CLANG) as wrappers that run them under cachegrind, and their instructions are the stages
`clang` and `linker`.

Every run has a fresh empty VELT_HOME and VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off, no other
VELT_* variable of the caller's but where the tools are, and runs on 4 cores (`taskset`), so the
counts and the peak don't depend on the machine's size. `--jobs N` runs N measurements at once,
on cores 0-3, 4-7, ... (instructions don't depend on load; the wall times then do). Each
measurement leaves result.json in its directory under --out, and `--reuse` reports and checks
those instead of measuring again.

`--check` compares with the budgets file (budgets.tsv next to this script):
- linear cost: between two sizes, every stage's instructions, the instructions in all, the
  functions and statements lowered, the IR lines and the peak memory may grow at most as fast
  as 2.1x per doubling of the lines, (n2/n1)^log2(2.1) (11.8x for ten times the lines); stages
  under 1% of the larger run's instructions are not compared;
- budgets: instructions per line per stage and in all, the peak memory and the counts of the
  code may exceed the recorded values by at most the file's tolerances (3%, 10% and 1%).
A line `superlinear <mode> <metric> <growth>` in the budgets file records a growth known today,
which may not get worse; fixing it removes the line (the list only shrinks). Exits 1 on a
failure, with the failing rows marked **over** in the table. `--write-budgets` writes the file
from this run (or, with `--reuse`, from the result.json files of another machine's run, such as
the nightly job's artifact).

Linux only (callgrind and taskset). Needs python3 and valgrind.
"""
import argparse
import concurrent.futures
import glob
import json
import math
import os
import queue
import re
import shutil
import statistics
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
# Native runs per measurement; the peak memory and the times are their median.
NATIVE_RUNS = 3
# Where callgrind dumps its counters: the end of each stage.
DUMP_AFTER = "*Session>::record_time"
# The processes callgrind does not follow (they are counted by --children, or not at all): every
# child but velt itself (the source generators' host).
SKIP_CHILDREN = "*clang*,*cc,*gcc*,*ld,*ld.*,*lld*,*collect2*,*mold*"
# Parts of `optimize`, by the inclusive instructions of their entry functions there.
OPTIMIZE_PARTS = [("numrep", "velt_opt::numrep::run"),
                  ("VIR verify", "velt_vir::verify::verify_program_on::{closure#1}")]


# The only VELT_* variables passed on from the caller's environment: where the tools are. Any
# other (VELT_THREADS, VELT_STD_OBJECT, VELT_LOWER_PROFILE, ...) could change what is measured.
KEPT_VELT_VARS = {"VELT_STD", "VELT_CLANG", "VELT_LLVM_BIN", "VELT_ZIG"}


def env_for(home):
    env = {k: v for k, v in os.environ.items()
           if not k.startswith("VELT_") or k in KEPT_VELT_VARS}
    env.update(VELT_HOME=home, VELT_BUILD_CACHE="off", VELT_BUILD_STAMP="off")
    env.setdefault("VELT_STD", os.path.join(ROOT, "std"))
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
        m = re.match(r"velt: (\S.*?)\s+[\d.]+ ms", line)
        if m and m.group(1) != "total":
            names.append(m.group(1))
    return names


def callgrind_parts(prefix):
    """The dumps of the main process, in order ([(path, instructions)]), the instructions of the
    source generators' host processes, and those of any other process callgrind followed."""
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
    host = sum(n for p, ps in by_pid.items() if p in hosts for _, _, n in ps)
    other = sum(n for p, ps in by_pid.items() if p not in hosts for _, _, n in ps)
    return parts, host, other


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


def stages_of(parts, names, host, other=0):
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
    if host:
        stages["generator host"] = host
    if other:
        stages["other processes"] = other
    return stages


def child_wrappers(work, path):
    """A directory of `clang`, `cc`, `ld` and `ld.lld` wrappers that run the real tool (found on
    `path`; clang at $VELT_CLANG if set) under cachegrind, writing to `<work>/children`. A
    wrapper follows the tool's children (cc's collect2) but not into another wrapper: cc running
    ld.lld from PATH starts that wrapper natively, which counts ld.lld itself."""
    bindir, outdir = os.path.join(work, "wrap"), os.path.join(work, "children")
    os.makedirs(bindir, exist_ok=True)
    os.makedirs(outdir, exist_ok=True)
    reals = {}
    for tool in ["clang", "cc", "ld.lld", "ld"]:
        real = shutil.which(tool, path=path)
        if tool == "clang" and os.environ.get("VELT_CLANG"):
            real = os.environ["VELT_CLANG"]
        if not real:
            continue
        reals[tool] = real
        kind = "clang" if tool == "clang" else "linker"
        with open(os.path.join(bindir, tool), "w") as f:
            f.write("#!/bin/sh\n"
                    "exec valgrind --tool=cachegrind --cache-sim=no --trace-children=yes "
                    f"--trace-children-skip='{bindir}/*' "
                    f'--cachegrind-out-file="$YARDSTICK_CHILDREN/{kind}.%p" -q {real} "$@"\n')
        os.chmod(os.path.join(bindir, tool), 0o755)
    return bindir, outdir, reals


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
    """Functions and statements lowered, summed over the passes and in the last pass (the
    program), and the number of passes."""
    fns = stmts = passes = 0
    last = (0, 0)
    pat = re.compile(r"velt-lower: pass \d+: [\d.]+ ms, (\d+) functions, \d+ blocks, (\d+) statements")
    for line in open(err_path, errors="replace"):
        m = pat.match(line)
        if m:
            last = (int(m.group(1)), int(m.group(2)))
            fns += last[0]
            stmts += last[1]
            passes += 1
    return {"functions": fns, "statements": stmts, "passes": passes,
            "functions_final": last[0], "statements_final": last[1]}


def count_lines(paths):
    n = 0
    for p in paths:
        with open(p, "rb") as f:
            n += sum(1 for _ in f)
    return n


def measure(velt, mode, size, src, lines, work, children, cores, ir_only=False):
    """One mode at one size; `ir_only` (release at a size not in --release-sizes) counts only
    the LLVM IR lines."""
    os.makedirs(work, exist_ok=True)
    res = {"mode": mode, "size": size, "lines": lines, "stages": {}}

    def home():
        return tempfile.mkdtemp(prefix="home.", dir=work)

    def pinned_cmd(cmd):
        return pinned(cmd, cores)

    def fail(what, err):
        tail = "".join(open(err, errors="replace").readlines()[-30:])
        sys.exit(f"{mode} at {size} lines: {what} failed\n{tail}")

    def save():
        with open(os.path.join(work, "result.json"), "w") as f:
            json.dump(res, f, indent=1)
        return res

    # Natively: time and peak memory, the median of NATIVE_RUNS runs (the peak of a build that
    # runs threads moves by a tenth or more from run to run).
    if not ir_only:
        runs = []
        for i in range(NATIVE_RUNS):
            err = os.path.join(work, f"native{i}.err")
            rc, wall, cpu, rss = run(pinned_cmd(velt_cmd(velt, mode, os.path.join(work, "app"))),
                                     src, env_for(home()), err)
            if rc != 0:
                fail("the native run", err)
            runs.append((wall, cpu, rss))
        res.update(wall=statistics.median(r[0] for r in runs),
                   cpu=statistics.median(r[1] for r in runs),
                   peak_mb=statistics.median(r[2] for r in runs),
                   peaks_mb=[round(r[2]) for r in runs])

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
        res.update(lower_counts(err))
        res["ir_lines"] = count_lines([vir])
        os.remove(vir)
    elif mode == "release":
        err = os.path.join(work, "llvm.err")
        ll = os.path.join(work, "app.ll")
        rc, *_ = run(pinned_cmd(velt_cmd(velt, mode, os.path.join(work, "app-ll"), ["--emit", "llvm"])),
                     src, env_for(home()), err, ll)
        if rc != 0:
            fail("--emit llvm", err)
        res["ir_lines"] = count_lines([ll])
        os.remove(ll)
        if ir_only:
            return save()

    # Instructions per stage.
    env = env_for(home())
    # The source generators' host runs under valgrind too, far slower than its time limit expects.
    env["VELT_GENERATOR_TIMEOUT"] = "36000"
    if children:
        bindir, outdir, reals = child_wrappers(work, env["PATH"])
        for old in glob.glob(os.path.join(outdir, "*")):
            os.remove(old)
        env["PATH"] = bindir + os.pathsep + env["PATH"]
        env["YARDSTICK_CHILDREN"] = outdir
        if "clang" in reals:
            env["VELT_CLANG"] = os.path.join(bindir, "clang")
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
    parts, host, other = callgrind_parts(prefix)
    names = stage_names(err)
    if len(parts) != len(names) + 1:
        sys.exit(f"{mode} at {size} lines: {len(parts)} callgrind dumps for {len(names)} stages "
                 f"({', '.join(names)}): is `Session::record_time` inlined now?")
    res["stages"] = stages_of(parts, names, host, other)
    if children:
        for kind, n in children_totals(outdir).items():
            res["stages"][kind] = n
    res["instructions"] = sum(res["stages"].values())
    return save()


# --- budgets -------------------------------------------------------------------------------

# The counts of the code: exact on every machine, so budgeted with almost no tolerance.
CODE = ("functions", "statements", "functions_final", "statements_final", "ir_lines")
# Of those, the ones compared between sizes.
CODE_GROWTH = ("functions", "statements", "ir_lines")


def kind(key):
    """The tolerance a metric's budget gets: instructions, peak memory or code."""
    return "instr" if key.startswith("instr:") else ("peak_mb" if key == "peak_mb" else "code")


def read_budgets(path):
    tol, budgets, superlinear = {"instr": 0.03, "peak_mb": 0.10, "code": 0.01}, {}, {}
    if not os.path.exists(path):
        return tol, budgets, superlinear
    for line in open(path):
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        f = line.split("\t")
        want = {"tolerance": 3, "budget": 5, "superlinear": 4}.get(f[0])
        if want != len(f):
            sys.exit(f"{path}: a line this script doesn't read: {line!r}")
        if f[0] == "tolerance":
            # tolerance <instr | peak_mb | code> <fraction>
            tol[f[1]] = float(f[2])
        elif f[0] == "budget":
            # budget <mode> <size> <metric> <value>
            budgets[(f[1], int(f[2]), f[3])] = float(f[4])
        elif f[0] == "superlinear":
            # superlinear <mode> <metric> <ratio per doubling>
            superlinear[(f[1], f[2])] = float(f[3])
    return tol, budgets, superlinear


def metrics(r):
    """The checked metrics of a run, by name: instructions per stage and in all, the peak
    memory and the counts of the code, whichever the run has."""
    m = {}
    for stage, n in r["stages"].items():
        m[f"instr:{stage}"] = n
    if "instructions" in r:
        m["instr:all"] = r["instructions"]
    for k in ("peak_mb",) + CODE:
        if k in r:
            m[k] = r[k]
    return m


def small_stage(key, v, r):
    """A stage under MIN_SHARE of its run's instructions: neither compared nor budgeted."""
    return key.startswith("instr:") and key != "instr:all" and v < MIN_SHARE * r["instructions"]


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
                if v1 is None or (kind(key) == "code" and key not in CODE_GROWTH):
                    continue
                if small_stage(key, v2, b):
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
            per = v / r["lines"] if key.startswith("instr:") else v
            b = budgets.get((r["mode"], r["size"], key))
            if b is None:
                continue
            t = tol[kind(key)]
            over = per > b * (1 + t)
            budget_rows.append((r["mode"], r["size"], key, per, b, over))
            if over:
                failures.append(f"{r['mode']} {r['size']} {key}: {per:.0f} over budget {b:.0f} (+{t:.0%})")
    return growth_rows, budget_rows, failures


def write_budgets(results, path, growth_rows):
    out = [
        "# Budgets of the build-speed yardstick (bench/compile/scale/yardstick.py, #2707).",
        "# Written by `yardstick.py --write-budgets` on Linux x86-64; edit only to tighten.",
        "# Written from the nightly runner's results (the job's `yardstick` artifact, then",
        "# `--reuse --write-budgets`): instruction counts differ by 1-2% between machines.",
        "# budget <mode> <size> <metric> <value>: instructions per source line (instr:<stage>,",
        "# instr:all), the peak resident set in MB (peak_mb, the median of three runs), or a",
        "# count of the code (functions, statements, IR lines), at most `tolerance` over.",
        "# superlinear <mode> <metric> <growth per doubling>: a metric that grows faster than (or",
        "# within 1% of) 2.1x per doubling today, recorded 1% over its growth; it may not get",
        "# worse, and the line goes once it is fixed.",
        "tolerance\tinstr\t0.03",
        "tolerance\tpeak_mb\t0.10",
        "tolerance\tcode\t0.01",
    ]
    # Growth within 1% of the limit is recorded too, 1% over what it is, so the nightly check
    # doesn't flip on a change that adds a few instructions somewhere.
    for (mode, key, _, _, _, _, d, _, _, _) in growth_rows:
        if d > DOUBLING * 0.99:
            out.append(f"superlinear\t{mode}\t{key}\t{d * 1.01:.2f}")
    for r in results:
        for key, v in metrics(r).items():
            if small_stage(key, v, r):
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
            # Stages under a million instructions in every run are left out.
            if s not in stages and any(x["stages"].get(s, 0) >= 1e6 for x in results):
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
            n = r.get("instructions") if s == "all" else r["stages"].get(s)
            cells.append("" if n is None else f"{n / 1e6:,.0f} ({n / r['lines']:,.0f})")
        name = f"**{s}**" if s == "all" else s
        out.append(f"| {name} | " + " | ".join(cells) + " |")
    out.append("")
    out.append("#### The code and the process")
    out.append("")
    out += [head.replace("| stage |", "| |"), "|---|" + "---:|" * len(results)]
    for key, fmt in [("lines", "{:,}"), ("functions", "{:,}"), ("statements", "{:,}"),
                     ("passes", "{}"), ("functions_final", "{:,}"), ("statements_final", "{:,}"),
                     ("ir_lines", "{:,}"), ("peak_mb", "{:,.0f}"), ("wall", "{:.1f}"),
                     ("cpu", "{:.1f}")]:
        label = {"functions": "functions lowered (all passes)",
                 "statements": "statements lowered (all passes)",
                 "functions_final": "functions in the program (last pass)",
                 "statements_final": "statements in the program (last pass)",
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
                    help="sizes measured in full for release builds (default: --sizes); at the "
                         "other sizes release builds count only the LLVM IR lines")
    ap.add_argument("--children", action="store_true")
    ap.add_argument("--jobs", type=int, default=1,
                    help="measurements at once, each on its own 4 cores")
    ap.add_argument("--budgets", default=os.path.join(HERE, "budgets.tsv"))
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--write-budgets", action="store_true")
    ap.add_argument("--out")
    ap.add_argument("--reuse", action="store_true",
                    help="take each measurement's result.json from an earlier run in --out")
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
        if os.path.exists(src) and not a.reuse:
            shutil.rmtree(src)
        made = subprocess.check_output([sys.executable, os.path.join(HERE, "app.py"), str(n), src],
                                       text=True)
        lines = int(made.split()[-2])
        for mode in modes:
            tasks.append((mode, n, src, lines, mode == "release" and n not in rel_sizes))
    # The longest first, so the runs at once end together.
    tasks.sort(key=lambda t: (t[4], -t[1], -modes.index(t[0])))
    cores = queue.Queue()
    for j in range(a.jobs):
        cores.put(f"{j * CORES_PER_JOB}-{j * CORES_PER_JOB + CORES_PER_JOB - 1}")

    def one(task):
        mode, n, src, lines, ir_only = task
        saved = os.path.join(out, f"app-{n}", mode, "result.json")
        if a.reuse and os.path.exists(saved):
            with open(saved) as f:
                return json.load(f)
        c = cores.get()
        try:
            what = " (IR lines only)" if ir_only else ""
            print(f"{mode} at {n} lines ({lines}){what}...", file=sys.stderr, flush=True)
            return measure(velt, mode, n, src, lines, os.path.join(out, f"app-{n}", mode),
                           a.children, c, ir_only)
        finally:
            cores.put(c)

    with concurrent.futures.ThreadPoolExecutor(max_workers=a.jobs) as pool:
        results = list(pool.map(one, tasks))
    results.sort(key=lambda r: (modes.index(r["mode"]), r["size"]))
    growth_rows, budget_rows, failures = check(results, a.budgets)
    text = report(results, growth_rows, budget_rows if a.check else [])
    print(text)
    if a.md:
        with open(a.md, "w") as f:
            f.write(text)
    if a.write_budgets:
        write_budgets(results, a.budgets, growth_rows)
        print(f"wrote {a.budgets}", file=sys.stderr)
    if a.check and failures:
        print("yardstick: " + "; ".join(failures), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
