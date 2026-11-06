#!/usr/bin/env python3
"""The mutants of instrument.py against the debug-variable test's sample, for real: each must make
some program of the sample fail. usage: mutants_on_sample.py <velt> <repo> <work> <every_n stride>"""
import concurrent.futures as cf, json, os, subprocess, sys
velt, repo, work, n = sys.argv[1:5]
n = int(n)
MUST = ["lang/array_join_enum.velt", "lang/closure_nested_assigns_capture.velt"]
gold = os.path.join(repo, "tests/golden")
def fnv(s):
    h = 0xcbf29ce484222325
    for b in s.encode():
        h = ((h ^ b) * 0x100000001b3) & (2**64 - 1)
    return h
files = []
for cat in sorted(os.listdir(gold)):
    d = os.path.join(gold, cat)
    if os.path.isdir(d):
        files += [f"{cat}/{x}" for x in sorted(os.listdir(d)) if x.endswith(".velt") and not x.startswith("_")]
sample = [f for f in files if fnv(f) % n == 0 or f in MUST]
print(f"{len(sample)} of {len(files)}", flush=True)
def run(rel, m):
    env = dict(os.environ, VELT_STD=os.path.join(repo, "std"), VELT_BUILD_STAMP="off", VELT_BUILD_CACHE=os.path.join(work, "cache"))
    if m:
        env["VELT_MUT"] = str(m)
    o = subprocess.run([velt, "build", "--emit", "vir", "--target", "x86_64-unknown-linux-gnu", os.path.join(gold, rel)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, errors="replace")
    return rel, o.returncode, ("ICE" in o.stderr or "panicked" in o.stderr or "VIR verification failed" in o.stderr)
with cf.ThreadPoolExecutor(8) as pool:
    base = {r: c for r, c, _ in pool.map(lambda r: run(r, None), sample)}
    good = [r for r in sample if base[r] == 0]
    print(f"compile unmutated: {len(good)}", flush=True)
    survived = []
    mutants = list(range(1, 63)) + [100, 101, 102, 103]
    unreachable = {9, 10, 11, 21, 35, 38, 62}
    for m in mutants:
        if m in unreachable:
            continue
        caught = sum(1 for r, c, ice in pool.map(lambda r: run(r, m), good) if ice or c != 0)
        print(f"mutant {m}: caught by {caught} of {len(good)}", flush=True)
        if not caught:
            survived.append(m)
    print("survived:", survived, flush=True)
