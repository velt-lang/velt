#!/usr/bin/env python3
"""A golden's debug build with variables described (`velt build --cpu native`, then the program)
against `velt run`: CPU seconds of each from its own empty build cache, and whether the program
prints its `.out` each way. usage: build_vs_run.py <velt> <repo> <work> <every_n>"""
import os, shutil, subprocess, sys
velt, repo, work, step = sys.argv[1:5]
step = int(step)
os.makedirs(work, exist_ok=True)
gold = os.path.join(repo, "tests/golden")
files = []
for cat in ["lang", "std", "m1", "m2", "m3", "async_dispatch", "bugs"]:
    d = os.path.join(gold, cat)
    files += [os.path.join(d, n) for n in sorted(os.listdir(d)) if n.endswith(".vlt") and not n.startswith("_") and os.path.exists(os.path.join(d, n[:-4] + ".out"))]
sample = files[::step]
def timed(cmd, env):
    p = subprocess.Popen(cmd, cwd=work, env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    out = p.stdout.read()
    _, status, ru = os.wait4(p.pid, 0)
    return ru.ru_utime + ru.ru_stime, out.replace(b"\r\n", b"\n"), os.waitstatus_to_exitcode(status)
def expect(f):
    code_file = f[:-4] + ".code"
    return (int(open(code_file).read().strip()) if os.path.exists(code_file) else 0,
            open(f[:-4] + ".out", "rb").read().replace(b"\r\n", b"\n"))
for round in range(2):
    for way in ["run", "build"]:
        cache = os.path.join(work, f"cache-{way}-{round}")
        shutil.rmtree(cache, ignore_errors=True)
        env = dict(os.environ, VELT_STD=os.path.join(repo, "std"), VELT_BUILD_CACHE=cache, VELT_RT_DEBUG_ALLOC="1", VELT_BUILD_STAMP="off")
        cpu = 0.0; same = 0; bad = []
        for f in sample:
            if way == "run":
                c, out, code = timed([velt, "run", f], env)
            else:
                exe = os.path.join(work, "p")
                c, _, bc = timed([velt, "build", "--cpu", "native", "-o", exe, f], env)
                c3, out, code = timed([exe], dict(env, VELT_SCRIPT=f)) if bc == 0 else (0, b"", -1)
                c += c3
            cpu += c
            if (code, out) == expect(f):
                same += 1
            else:
                bad.append(os.path.relpath(f, gold))
        print(f"round {round} {way:5s}: {len(sample)} programs, {cpu/len(sample):.2f} CPU s each; prints its .out: {same}; not: {bad}", flush=True)
