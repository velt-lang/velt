#!/usr/bin/env python3
"""Can goldens share a binary? Builds a sample of goldens one by one and as modules of one program
(a generated `main` dispatches on GOLDEN_INDEX, as `velt test` compiles a directory into one
program), compares CPU seconds and every program's stdout and exit code.
usage: merge_try.py <velt> <repo> <work> <dir under tests/golden> <count> <mode> [stride]"""
import os, re, resource, subprocess, sys, shutil
velt, repo, work, sub, count, mode = sys.argv[1:7]
stride = int(sys.argv[7]) if len(sys.argv) > 7 else 1
count = int(count)
gold = os.path.join(repo, "tests/golden", sub)
os.makedirs(work, exist_ok=True)
gen = os.path.join(work, f"gen-{mode}")
shutil.rmtree(gen, ignore_errors=True); os.makedirs(gen)
names = sorted(n[:-4] for n in os.listdir(gold) if n.endswith(".vlt") and not n.startswith("_"))
picked, skipped = [], {}
for n in names:
    p = os.path.join(gold, n)
    src = open(p + ".vlt", encoding="utf8").read()
    why = None
    if not os.path.exists(p + ".out"): why = "compile-error golden"
    elif re.search(r'^\s*(import|export)\b.*from\s+"\.', src, re.M): why = "relative import"
    elif os.path.exists(p + ".stderr"): why = "stderr expectation"
    elif re.search(r'^// check: (heap|also)', src, re.M): why = "per-program count"
    elif not re.search(r'^(async\s+)?function main\s*\(', src, re.M): why = "no plain main"
    elif re.search(r'^(?!function|async|class|interface|type|enum|const|let|var|import|export|declare|abstract|//|/\*|\s|\}|\)|\]|@|$|\*)', src, re.M): why = "top-level statements"
    if why: skipped[why] = skipped.get(why, 0) + 1; continue
    picked.append(n)
picked = picked[::stride][:count]
print(f"{sub}: {len(names)} files, picked {len(picked)}; left out: {skipped}")
def env():
    return dict(os.environ, VELT_BUILD_STAMP="off", VELT_STD=os.path.join(repo, "std"), VELT_BUILD_CACHE="off")
args = ["--release"] if mode == "release" else []
def run(cmd, **kw):
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    o = subprocess.run(cmd, capture_output=True, text=True, errors="replace", timeout=3000, **kw)
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    return o, r1.ru_utime + r1.ru_stime - r0.ru_utime - r0.ru_stime
single_cpu = 0.0
for n in picked:
    o, c = run([velt, "build", "-o", os.path.join(gen, "single")] + args + [os.path.join(gold, n + ".vlt")], env=env(), cwd=gen)
    single_cpu += c
    if o.returncode: print("single build failed", n, o.stderr[:300])
print(f"one by one: {single_cpu:.1f} s CPU for {len(picked)} ({single_cpu/len(picked):.2f} per program)")
imports, arms = [], []
for i, n in enumerate(picked):
    src = open(os.path.join(gold, n + ".vlt"), encoding="utf8").read()
    is_async = re.search(r'^async\s+function main\s*\(', src, re.M) is not None
    src = re.sub(r'^(async\s+)?function main\s*\(', lambda m: "export " + (m.group(1) or "") + "function main(", src, count=1, flags=re.M)
    open(os.path.join(gen, f"g{i}.vlt"), "w", encoding="utf8").write(src)
    imports.append(f'import * as g{i} from "./g{i}.vlt";')
    arms.append(f'  if (k == "{i}") {{ {"await " if is_async else ""}g{i}.main(); return; }}')
open(os.path.join(gen, "all.vlt"), "w", encoding="utf8").write("\n".join(imports) + '\nasync function main() {\n  const k = process.env["GOLDEN_INDEX"] ?? "";\n' + "\n".join(arms) + "\n}\n")
o, merged_cpu = run([velt, "build", "--timings", "-o", os.path.join(gen, "all")] + args + [os.path.join(gen, "all.vlt")], env=env(), cwd=gen)
print(f"merged: {merged_cpu:.1f} s CPU, exit {o.returncode}")
print("\n".join(l for l in o.stderr.splitlines() if re.match(r"velt: \w", l))[:1500])
if o.returncode:
    print(o.stderr[:3000]); sys.exit(0)
ok, bad = 0, []
for i, n in enumerate(picked):
    want = open(os.path.join(gold, n + ".out"), encoding="utf8").read()
    code_file = os.path.join(gold, n + ".code")
    want_code = int(open(code_file).read().strip()) if os.path.exists(code_file) else 0
    try:
        r = subprocess.run([os.path.join(gen, "all")], capture_output=True, text=True, errors="replace", timeout=120, cwd=gen, env=dict(os.environ, GOLDEN_INDEX=str(i)))
        got = (r.returncode, r.stdout)
    except subprocess.TimeoutExpired:
        got = (-99, "")
    if got == (want_code, want): ok += 1
    else: bad.append(n)
print(f"same output and exit code: {ok} of {len(picked)}; different: {bad[:30]}")
print(f"saved {100*(1-merged_cpu/single_cpu):.0f}% of the compile CPU")
