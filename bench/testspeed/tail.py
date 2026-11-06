#!/usr/bin/env python3
"""CPU seconds of each golden's build, debug and release, to see how much a few programs cost.
usage: tail.py <velt> <std> <goldens> <work> <every_n> <workers>"""
import concurrent.futures as cf, json, os, re, subprocess, sys, time
velt, std, gold, work, step, workers = sys.argv[1:7]
step, workers = int(step), int(workers)
os.makedirs(work, exist_ok=True)
files = []
for top, dirs, names in os.walk(gold):
    dirs[:] = sorted(d for d in dirs if not d.startswith("_"))
    for n in sorted(names):
        p = os.path.join(top, n)
        if n.endswith(".velt") and not n.startswith("_") and n != "package.velt" and os.path.exists(p[:-4] + ".out"):
            files.append(p)
files.sort()
sample = files[::step]
def one(job):
    i, f = job
    out = {}
    for mode in ["debug", "release"]:
        env = dict(os.environ, VELT_STD=std, VELT_BUILD_STAMP="off", VELT_BUILD_CACHE=os.path.join(work, "cache"))
        err = os.path.join(work, f"err{i}-{mode}")
        with open(err, "w") as e:
            p = subprocess.Popen([velt, "build", "--timings", "-o", f"{work}/p{i}-{mode}"] + (["--release"] if mode == "release" else []) + [f], env=env, stdout=subprocess.DEVNULL, stderr=e, cwd=work)
            _, status, ru = os.wait4(p.pid, 0)
        stderr = open(err).read()
        class O: pass
        o = O(); o.stderr = stderr; o.returncode = status
        last = [ru.ru_utime, ru.ru_stime]
        stages = {m.group(1): float(m.group(2)) for m in re.finditer(r"^velt: (\w+)\s+([\d.]+) ms", o.stderr, re.M)}
        out[mode] = {"cpu": float(last[0]) + float(last[1]), "ok": o.returncode == 0, "stages": stages}
    return os.path.relpath(f, gold), out
rows = []
with cf.ThreadPoolExecutor(workers) as pool:
    for r in pool.map(one, enumerate(sample)):
        rows.append(r)
json.dump(rows, open(os.path.join(work, "tail.json"), "w"))
for mode in ["debug", "release"]:
    ok = sorted(((o[mode]["cpu"], n) for n, o in rows if o[mode]["ok"]), reverse=True)
    total = sum(c for c, _ in ok)
    print(f"\n{mode}: {len(ok)} programs, {total:.0f} s CPU, mean {total/len(ok):.2f}, median {ok[len(ok)//2][0]:.2f}")
    for frac in (0.01, 0.05, 0.1, 0.25):
        k = max(1, int(len(ok) * frac))
        print(f"  the slowest {100*frac:.0f}% ({k}) take {100*sum(c for c,_ in ok[:k])/total:.0f}% of the CPU")
    for c, n in ok[:25]:
        print(f"  {c:7.1f} {n}")
