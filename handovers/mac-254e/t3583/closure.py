"""Import closure of a std file (relative and velt/ imports), with the paths that reach a target.

usage: closure.py <std dir> <start file> <target module path, e.g. lang/hooks/sched_all>
"""
import os
import re
import sys
from collections import deque

std, start, target = sys.argv[1], sys.argv[2], sys.argv[3]
IMP = re.compile(r'^\s*(?:import|export)\b[^"\n]*?from\s+"([^"]+)"', re.M)


def resolve(frm, spec):
    if spec.startswith("./") or spec.startswith("../"):
        p = os.path.normpath(os.path.join(os.path.dirname(frm), spec))
    elif spec.startswith("velt/"):
        p = os.path.join(std, spec[len("velt/"):])
    else:
        return None
    for cand in (p + ".velt", os.path.join(p, "index.velt")):
        if os.path.isfile(cand):
            return cand
    return None


def rel(p):
    return os.path.relpath(p, std)[: -len(".velt")]


seen = {start: None}
todo = deque([start])
while todo:
    f = todo.popleft()
    for spec in IMP.findall(open(f).read()):
        g = resolve(f, spec)
        if g and g not in seen:
            seen[g] = f
            todo.append(g)
print(f"{len(seen)} modules in the closure")
for name in ("lang/hooks/sched", target, "lang/hooks/sched_workers"):
    hit = [p for p in seen if rel(p) == name]
    if not hit:
        print(f"{name}: not reached")
        continue
    chain, p = [], hit[0]
    while p:
        chain.append(rel(p))
        p = seen[p]
    print(f"{name}: " + " <- ".join(chain))
