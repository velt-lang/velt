"""Where a golden's compile time goes: `velt build --timings` over a sample of goldens with the
worktree's debug velt. Each program is built in place, once per pass, to an output path of its
own (so no build is skipped as up to date); the build cache is shared, so std's functions come
from it after the first programs, as in a golden run. Passes, in order:
  lazy          debug build, lazy std
  units         debug build from std's unit on disk
  release       --release, lazy std
  release-obj   --release with VELT_STD_OBJECT=1 (std's functions from the std cache), first pass
  release-obj2  the same again (the std cache warm for every program)
usage: prof.py <debug count> <release count>   -> scratchpad/prof/result.json, a table on stdout"""
import json, os, pathlib, re, subprocess, sys, statistics

W = pathlib.Path("/Users/andii/dev/velt/branches/d1-open-pr")
S = pathlib.Path(__file__).parent / "prof"
S.mkdir(exist_ok=True)
VELT = str(W / "target/debug/velt")
PHASE = re.compile(r"^velt: ([a-z][a-z ]*?)\s+([0-9.]+) ms")
INSTR = re.compile(r"^\s*(\d+)\s+instructions retired", re.M)
count, rel_count = int(sys.argv[1]), int(sys.argv[2])

sample = []
for line in (W / "../handovers/mac-e688-2943/sample300.txt").resolve().read_text().split():
    p = pathlib.Path(line)
    parts = p.relative_to(W).parts
    if not p.exists() or "errors" in parts or any(x.startswith("_") for x in parts):
        continue
    if p.with_suffix(".out").exists():
        sample.append(p)
sample = sample[:count]

base = {k: v for k, v in os.environ.items() if not k.startswith("VELT_")}
base["SDKROOT"] = "/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk"
base["VELT_STD"] = str(W / "std")
units = dict(base, VELT_UNITS="2", VELT_UNITS_SPLIT="1", VELT_UNITS_BUILD="1", VELT_UNITS_DISK=str(S / "unit"))
obj = dict(base, VELT_STD_OBJECT="1")
PASSES = [
    ("lazy", base, [], count),
    ("units", units, [], count),
    ("release", base, ["--release"], rel_count),
    ("release-obj", obj, ["--release"], rel_count),
    ("release-obj2", obj, ["--release"], rel_count),
]


def build(p, env, extra, out):
    cmd = ["/usr/bin/time", "-l", VELT, "build", p.name, "-o", str(out), "--timings"] + extra
    r = subprocess.run(cmd, cwd=p.parent, env=env, capture_output=True, text=True)
    phases = {}
    for l in r.stderr.splitlines():
        m = PHASE.match(l)
        if m:
            phases[m.group(1).strip()] = float(m.group(2))
    m = INSTR.search(r.stderr)
    if r.returncode != 0 or "total" not in phases or "sema" not in phases:
        return None
    return {"phases": phases, "instr": int(m.group(1)) if m else 0}


results = {}
for mode, env, extra, n in PASSES:
    out_dir = S / ("out-" + mode)
    out_dir.mkdir(exist_ok=True)
    rs = {}
    for i, p in enumerate(sample[:n]):
        r = build(p, env, extra, out_dir / ("p%d" % i))
        if r:
            rs[str(p.relative_to(W / "tests/golden"))] = r
    results[mode] = rs
    print("... pass %s: %d of %d built" % (mode, len(rs), min(n, len(sample))), file=sys.stderr, flush=True)
(S / "result.json").write_text(json.dumps(results))

ORDER = ["stamp", "parse", "sema", "generate", "lower", "verify", "std cache", "optimize", "codegen", "write", "link"]
for mode, rs in results.items():
    if not rs:
        continue
    tot = {}
    for r in rs.values():
        for k, v in r["phases"].items():
            tot[k] = tot.get(k, 0) + v
    total = tot["total"]
    ins = [r["instr"] for r in rs.values()]
    print("\n== %s: %d programs, %.0f ms and %.2f G instructions per program (median %.2f G)" % (
        mode, len(rs), total / len(rs), sum(ins) / len(ins) / 1e9, statistics.median(ins) / 1e9))
    for k in ORDER + sorted(set(tot) - set(ORDER) - {"total"}):
        if k in tot:
            print("  %-10s %7.1f ms  %5.1f%%" % (k, tot[k] / len(rs), tot[k] * 100 / total))


def versus(a, b):
    both = sorted(set(results.get(a, {})) & set(results.get(b, {})))
    if not both:
        return
    ia = sum(results[a][n]["instr"] for n in both)
    ib = sum(results[b][n]["instr"] for n in both)
    ta = sum(results[a][n]["phases"]["total"] for n in both)
    tb = sum(results[b][n]["phases"]["total"] for n in both)
    cheaper = sum(1 for n in both if results[b][n]["instr"] < results[a][n]["instr"])
    print("\n%s vs %s over %d programs: instructions %.1f G -> %.1f G (%+.1f%%), compile time %.1f s -> %.1f s (%+.1f%%); cheaper for %d" % (
        b, a, len(both), ia / 1e9, ib / 1e9, (ib - ia) * 100 / ia, ta / 1000, tb / 1000, (tb - ta) * 100 / ta, cheaper))


versus("lazy", "units")
versus("release", "release-obj")
versus("release", "release-obj2")
