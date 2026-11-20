"""passes.py <golden.velt> [n]: per-pass sema timings (VELT_SEMA_TIMINGS totals; the minimum of
n builds, default 5) of a lazy build and of a build from std's unit on disk, with
branches/d1-open-pr's debug velt. Each build goes to an output path of its own."""
import os, pathlib, re, subprocess, sys

W = pathlib.Path("/Users/andii/dev/velt/branches/d1-open-pr")
S = pathlib.Path(__file__).parent
src = pathlib.Path(sys.argv[1]).resolve()
n = int(sys.argv[2]) if len(sys.argv) > 2 else 5
out = S / "passes"
out.mkdir(exist_ok=True)
f = out / src.name
f.write_text(src.read_text())
base = {k: v for k, v in os.environ.items() if not k.startswith("VELT_")}
base.update(SDKROOT="/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk", VELT_STD=str(W / "std"), VELT_SEMA_TIMINGS="1")
units = dict(base, VELT_UNITS="2", VELT_UNITS_SPLIT="1", VELT_UNITS_BUILD="1", VELT_UNITS_DISK=str(S / "prof" / "unit"))
LINE = re.compile(r"^velt-sema:   (.*?)\s+([0-9.]+) ms\s+x(\d+)\s*$")
res = {}
for mode, env in (("lazy", base), ("units", units)):
    best = {}
    for i in range(n + 1):
        # A comment edit, so no build is answered from what an earlier one kept; the first is a warm-up.
        with open(f, "a") as h:
            h.write("// %s %d\n" % (mode, i))
        r = subprocess.run([str(W / "target/debug/velt"), "build", f.name, "-o", str(out / ("o-%s-%d" % (mode, i)))],
                           cwd=f.parent, env=env, capture_output=True, text=True)
        totals = False
        for l in r.stderr.splitlines():
            if l.startswith("velt-sema: totals"):
                totals = True
                continue
            m = LINE.match(l) if totals else None
            if m and i > 0:
                k, v = m.group(1), float(m.group(2))
                best[k] = min(best.get(k, 1e9), v)
    res[mode] = best
keys = sorted(set(res["lazy"]) | set(res["units"]), key=lambda k: -max(res["lazy"].get(k, 0), res["units"].get(k, 0)))
print("%-40s %8s %8s" % ("pass (min of %d, ms)" % n, "lazy", "units"))
for k in keys[:34]:
    print("%-40s %8.1f %8.1f" % (k, res["lazy"].get(k, 0), res["units"].get(k, 0)))
