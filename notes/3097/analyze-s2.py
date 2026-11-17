"""Summarize results-s2.txt: instruction deltas per async row (min of rounds), actors calls rows
(best wall of 5 rounds, cores busy = (user+sys)/wall), and the bench lines side by side."""
import re
import sys
from collections import defaultdict

path = sys.argv[1]
ir = defaultdict(list)
calls = defaultdict(list)
bench = defaultdict(list)
cur = None
for line in open(path, encoding="utf-8", errors="replace"):
    line = line.rstrip("\n")
    if line.startswith("build ") and "failed" in line:
        print("!!", line)
    m = re.match(r"IR (\d) (\S+) (\S+) (base|head) (\d+)$", line)
    if m:
        ir[(m[2], m[3], m[4])].append(int(m[5]))
        continue
    m = re.match(r"CALLS (\d) (\S+) (c=\d+) (base|head) (\d+) ms \| ([\d.]+) ([\d.]+) ([\d.]+)", line)
    if m:
        wall, user, sys_ = float(m[6]), float(m[7]), float(m[8])
        calls[(m[2], m[3], m[4])].append((int(m[5]), (user + sys_) / wall if wall else 0))
        continue
    if line.startswith("CALLS"):
        print("?? unparsed", line)
    m = re.match(r"== BENCH (\d) (base|head)", line)
    if m:
        cur = m[2]
        continue
    if cur and line.strip():
        bench[cur].append(line.strip())

print("== instructions (min of rounds), head vs base")
worst = []
for (cfg, name, side) in sorted(ir):
    if side != "base":
        continue
    b = min(ir[(cfg, name, "base")])
    h = ir.get((cfg, name, "head"))
    if not h or not b:
        continue
    d = (min(h) - b) / b * 100
    worst.append((abs(d), cfg, name, d))
    print(f"{cfg:3} {name:28} {b:>14,} {min(h):>14,} {d:+6.2f}%")
worst.sort(reverse=True)
print("largest:", [(c, n, round(d, 2)) for _, c, n, d in worst[:6]])

print("\n== actors calls, 4 shards (best ms of 5; cores busy median)")
for (sc, cc, side) in sorted(calls):
    if side != "base":
        continue
    for s in ("base", "head"):
        rows = calls[(sc, cc, s)]
        ms = sorted(r[0] for r in rows)
        cores = sorted(r[1] for r in rows)
        print(f"{sc:5} {cc:6} {s}: best {ms[0]} ms, median {ms[len(ms)//2]} ms, cores {cores[len(cores)//2]:.2f}  all={ms}")

print("\n== bench --shards 4 (two runs each, in order)")
for s in ("base", "head"):
    print(f"-- {s}")
    for l in bench[s]:
        print("  ", l)
