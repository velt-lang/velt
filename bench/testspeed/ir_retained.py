#!/usr/bin/env python3
"""What a program's LLVM IR is made of: lines per symbol, and what each symbol retains (the lines
that go away if nothing refers to it: the dominator tree of the reference graph from `velt_main`).
usage: velt build [--release] --emit llvm x.velt > x.ll; ir_retained.py x.ll [top]"""
import collections, re, sys

path = sys.argv[1]
top = int(sys.argv[2]) if len(sys.argv) > 2 else 40
SYM = re.compile(r'@("[^"]+"|[-\w$.]+)')
size, refs = collections.Counter(), collections.defaultdict(set)
cur = None
for line in open(path, errors="replace"):
    if cur is not None:
        size[cur] += 1
        refs[cur].update(m.group(1).strip('"') for m in SYM.finditer(line))
        if line.startswith("}"):
            cur = None
    elif line.startswith("define "):
        cur = re.search(r'@("[^"]+"|[^\s(]+)\(', line).group(1).strip('"')
        size[cur] += 1
    elif line.startswith("@"):
        m = SYM.match(line)
        g = m.group(1).strip('"')
        size[g] += 1
        refs[g].update(x.group(1).strip('"') for x in SYM.finditer(line[m.end():]))

ROOT = "<root>"
refs[ROOT] = {s for s in size if s in ("velt_main", "main") or s.startswith("_Gstatics")}
order, seen, stack = [], {ROOT}, [(ROOT, iter(sorted(refs[ROOT])))]
while stack:
    n, it = stack[-1]
    for c in it:
        if c not in seen and c in size:
            seen.add(c)
            stack.append((c, iter(sorted(refs.get(c, ())))))
            break
    else:
        order.append(n)
        stack.pop()
index = {n: i for i, n in enumerate(order)}
pred = collections.defaultdict(list)
for n in seen:
    for c in refs.get(n, ()):
        if c in seen:
            pred[c].append(n)
idom = {ROOT: ROOT}


def meet(a, b):
    while a != b:
        while index[a] < index[b]:
            a = idom[a]
        while index[b] < index[a]:
            b = idom[b]
    return a


changed = True
while changed:
    changed = False
    for n in order[-2::-1]:
        new = None
        for p in pred[n]:
            if p in idom:
                new = p if new is None else meet(new, p)
        if idom.get(n) != new:
            idom[n] = new
            changed = True
retained = collections.Counter()
for n in order:
    if n != ROOT:
        retained[n] += size[n]
        retained[idom[n]] += retained[n]
kinds, kind_count = collections.Counter(), collections.Counter()
for n, c in size.items():
    m = re.match(r"_G([a-z]+)", n)
    k = "_G" + m.group(1) if m else "(not glue)"
    kinds[k] += c
    kind_count[k] += 1
print(f"{sum(size.values())} lines in definitions, {retained[ROOT]} reached")
print("--- lines by glue kind")
for k, c in kinds.most_common(top):
    print(f"{c:9d} {kind_count[k]:6d} {k}")
print("--- retained, own lines, symbol")
for n, c in retained.most_common(top):
    print(f"{c:9d} {size[n]:7d} {n[:120]}")
