#!/usr/bin/env python3
"""Which debug-variable branches each golden reaches, with an instrumented `velt` (see
instrument.py): the program x branch matrix says what a sample of the goldens would catch of the
mutants that break a branch, and data mutants run for real on a sample and on the corpus part.
usage: debug_vars_coverage.py <velt> <repo> <work> <every_n> <workers> [mutants: 100,101,...]"""
import concurrent.futures as cf, hashlib, json, os, subprocess, sys
velt, repo, work, step, workers = sys.argv[1:6]
step, workers = int(step), int(workers)
mutants = [int(m) for m in sys.argv[6].split(",")] if len(sys.argv) > 6 else []
gold = os.path.join(repo, "tests/golden")
os.makedirs(work, exist_ok=True)
files = []
for cat in sorted(os.listdir(gold)):
    d = os.path.join(gold, cat)
    if not os.path.isdir(d):
        continue
    for n in sorted(os.listdir(d)):
        if n.endswith(".velt") and not n.startswith("_"):
            files.append(f"{cat}/{n}")
sample = files[::step]
print(f"{len(files)} goldens as the test finds them, running {len(sample)}", flush=True)
def compile_one(rel, mutant=None):
    h = hashlib.sha1(f"{rel}{mutant}".encode()).hexdigest()
    cov = os.path.join(work, h)
    if os.path.exists(cov):
        os.remove(cov)
    env = dict(os.environ, VELT_STD=os.path.join(repo, "std"), VELT_BUILD_STAMP="off", VELT_DBGCOV=cov)
    env["VELT_BUILD_CACHE"] = os.path.join(work, "cache")
    if mutant:
        env["VELT_MUT"] = str(mutant)
    o = subprocess.run([velt, "build", "--emit", "vir", "--target", "x86_64-unknown-linux-gnu", os.path.join(gold, rel)],
                       env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, errors="replace")
    marks = sorted({int(l) for l in open(cov)}) if os.path.exists(cov) else []
    if os.path.exists(cov):
        os.remove(cov)
    ice = "ICE" in o.stderr or "panicked" in o.stderr or "VIR verification failed" in o.stderr
    return rel, marks, o.returncode, ice
rows = {}
with cf.ThreadPoolExecutor(workers) as pool:
    for rel, marks, code, ice in pool.map(compile_one, sample):
        rows[rel] = {"marks": marks, "code": code, "ice": ice}
json.dump(rows, open(os.path.join(work, "matrix.json"), "w"))
compiled = [r for r in rows if rows[r]["code"] == 0]
ices = [r for r in rows if rows[r]["ice"]]
print(f"compiled {len(compiled)}, ICE on the unmutated compiler: {ices[:10]}", flush=True)
counts = {}
for r in rows:
    for m in rows[r]["marks"]:
        counts[m] = counts.get(m, 0) + 1
print("programs reaching each branch:", dict(sorted(counts.items())), flush=True)
print("branches no program reaches:", [m for m in range(1, 63) if m not in counts], flush=True)
print("least reached:", sorted(counts.items(), key=lambda kv: kv[1])[:20], flush=True)
print("any branch only std reaches? hello:", rows.get("m1/hello.velt", {}).get("marks"), flush=True)
for m in mutants:
    caught = []
    with cf.ThreadPoolExecutor(workers) as pool:
        for rel, marks, code, ice in pool.map(lambda r: compile_one(r, m), compiled):
            if ice or code != 0:
                caught.append(rel)
    print(f"data mutant {m}: caught by {len(caught)} of {len(compiled)}", caught[:5], flush=True)
