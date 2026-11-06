#!/usr/bin/env python3
"""Where a golden's compile goes: `velt build --timings` over every n-th golden, CPU seconds of
the process and the wall-clock stages it prints. usage: stages.py <velt> <repo> <work> [every_n] [modes]"""
import os, re, resource, subprocess, sys, collections, statistics, json
velt, repo, work = sys.argv[1:4]
step = int(sys.argv[4]) if len(sys.argv) > 4 else 40
modes = sys.argv[5].split(",") if len(sys.argv) > 5 else ["debug", "release"]
caches = sys.argv[6].split(",") if len(sys.argv) > 6 else ["off"]
os.makedirs(work, exist_ok=True)
files = []
for top, dirs, names in os.walk(os.path.join(repo, "tests/golden")):
    dirs[:] = sorted(d for d in dirs if not d.startswith("_"))
    for n in sorted(names):
        p = os.path.join(top, n)
        if n.endswith(".velt") and not n.startswith("_") and n != "package.velt" and os.path.exists(p[:-4] + ".out"):
            files.append(p)
files.sort()
sample = files[::step]
print(f"{len(files)} goldens with .out, sampling {len(sample)}")
for cache in caches:
    for mode in modes:
        totals = collections.Counter(); cpu = 0.0; n = 0; per = []
        for f in sample:
            env = dict(os.environ, VELT_BUILD_STAMP="off", VELT_STD=os.path.join(repo, "std"))
            if cache == "off":
                env["VELT_BUILD_CACHE"] = "off"
            else:
                env["VELT_BUILD_CACHE"] = os.path.join(work, "cache")
            exe = os.path.join(work, "p")
            cmd = [velt, "build", "--timings", "-o", exe] + (["--release"] if mode == "release" else []) + [f]
            r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
            o = subprocess.run(cmd, env=env, capture_output=True, text=True, cwd=work)
            r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
            c = r1.ru_utime + r1.ru_stime - r0.ru_utime - r0.ru_stime
            if o.returncode:
                continue
            n += 1; cpu += c; per.append(c)
            for m in re.finditer(r"^velt: (\w[\w ]*?)\s+([\d.]+) ms", o.stderr, re.M):
                totals[m.group(1)] += float(m.group(2))
        print(f"\n[{mode}, cache {cache}] {n} programs: CPU {cpu/n:.2f} s per program (median {statistics.median(per):.2f}, max {max(per):.2f})")
        tot = totals["total"] or 1
        for k, v in totals.most_common():
            print(f"  {k:12s} {v/n:9.1f} ms/program wall  {100*v/tot:5.1f}%")
