#!/usr/bin/env python3
"""How many goldens' release IR stays the same between two compilers (a persistent build cache's
hit rate: clang's object is keyed by the IR text), and how many of their Cranelift functions.
usage: ir_stability.py <veltA> <stdA> <veltB> <stdB> <goldens> <every_n>"""
import hashlib, os, subprocess, sys
va, sa, vb, sb, gold, step = sys.argv[1:7]
step = int(step)
files = []
for top, dirs, names in os.walk(gold):
    dirs[:] = sorted(d for d in dirs if not d.startswith("_"))
    for n in sorted(names):
        p = os.path.join(top, n)
        if n.endswith(".vlt") and not n.startswith("_") and n != "package.vlt" and os.path.exists(p[:-4] + ".out"):
            files.append(p)
files.sort()
sample = files[::step]
def ir(velt, std, f, release):
    env = dict(os.environ, VELT_STD=std, VELT_BUILD_CACHE="off", VELT_BUILD_STAMP="off")
    args = [velt, "build", "--emit", "llvm"] + (["--release"] if release else []) + [f]
    o = subprocess.run(args, capture_output=True, env=env)
    return hashlib.sha256(o.stdout).hexdigest() if o.returncode == 0 else None
same = differ = failed = 0
different = []
for f in sample:
    a, b = ir(va, sa, f, True), ir(vb, sb, f, True)
    if a is None or b is None:
        failed += 1
    elif a == b:
        same += 1
    else:
        differ += 1
        different.append(os.path.relpath(f, gold))
print(f"sample {len(sample)}: identical release IR {same}, different {differ}, not built {failed}")
print("different:", different[:20])
