#!/usr/bin/env python3
"""Compare two lowering passes (`VELT_LOWER_DUMP_PASSES=<dir>` writes pass<N>.vir per pass) function by function: a function is the same when its text is
equal once function, static, extern and agg numbers are left out. Prints counts and the
statement share of functions that differ."""
import re, sys
def funcs(path):
    out, cur, name = {}, [], None
    for line in open(path):
        if line.startswith("fn#"):
            if name: out[name] = cur
            m = re.match(r"fn#\d+ \w+ ([^(]+)\(", line)
            name, cur = m.group(1), [re.sub(r"^fn#\d+ ", "", line)]
        elif name is not None:
            cur.append(line)
    if name: out[name] = cur
    return out
norm = lambda ls: [re.sub(r"(fn|static|extern|agg)#\d+", r"\1#", l) for l in ls]
a, b = funcs(sys.argv[1]), funcs(sys.argv[2])
same = diff = only_a = only_b = 0; diff_lines = same_lines = 0
for k, v in b.items():
    if k not in a: only_b += 1; diff_lines += len(v); continue
    if norm(a[k]) == norm(v): same += 1; same_lines += len(v)
    else: diff += 1; diff_lines += len(v)
only_a = sum(1 for k in a if k not in b)
print(f"pass-1 functions: {len(b)}; same as pass 0: {same} ({same_lines} lines), differ: {diff}, new: {only_b} ({diff_lines} lines); pass-0 only: {only_a}")
print(f"reusable share of pass-1 lines: {same_lines/(same_lines+diff_lines):.1%}")
