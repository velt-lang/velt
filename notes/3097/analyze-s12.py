"""Summarize results-s12.txt (run-s12.sh): instruction deltas head vs base per async row (min of
rounds); the cross-worker round trip, idle CPU, actors calls (best/median ms, cores busy) and the
bench lines for the sides base, s2 (step 2) and head (steps 1+2)."""
import re
import sys
from collections import defaultdict

SIDES = ("base", "s2", "head")
path = sys.argv[1]
ir = defaultdict(list)
xw = defaultdict(list)
idle = defaultdict(list)
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
    m = re.match(r"XWAKE (\d) (\S+) workers \d+: round trip (\d+) ns", line)
    if m:
        xw[m[2]].append(int(m[3]))
        continue
    m = re.match(r"IDLE (\d) (\S+) \| ([\d.]+) ([\d.]+) ([\d.]+)", line)
    if m:
        idle[m[2]].append(float(m[4]) + float(m[5]))
        continue
    m = re.match(r"CALLS (\d) (\S+) (c=\d+) (\S+) (\d+) ms \| ([\d.]+) ([\d.]+) ([\d.]+)", line)
    if m:
        wall, user, sys_ = float(m[6]), float(m[7]), float(m[8])
        calls[(m[2], m[3], m[4])].append((int(m[5]), (user + sys_) / wall if wall else 0))
        continue
    if line.startswith(("CALLS", "XWAKE", "IDLE")):
        print("?? unparsed", line)
    m = re.match(r"== BENCH (\d) (\S+)", line)
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
print("largest:", [(c, n, round(d, 2)) for _, c, n, d in worst[:8]])

print("\n== cross-worker round trip, ns (all rounds)")
for s in SIDES:
    v = sorted(xw[s])
    if v:
        print(f"{s:5} best {v[0]} median {v[len(v)//2]}  all={v}")

print("\n== idle CPU, s user+sys over 5 s (ticker every 10 ms)")
for s in SIDES:
    print(f"{s:5} {idle[s]}")

print("\n== actors calls, 4 shards (best ms of 5; median; cores busy median)")
for key in sorted({(sc, cc) for (sc, cc, _) in calls}):
    for s in SIDES:
        rows = calls[(key[0], key[1], s)]
        if not rows:
            continue
        ms = sorted(r[0] for r in rows)
        cores = sorted(r[1] for r in rows)
        print(f"{key[0]:5} {key[1]:6} {s:5}: best {ms[0]} ms, median {ms[len(ms)//2]} ms, cores {cores[len(cores)//2]:.2f}  all={ms}")

print("\n== bench --shards 4 (two runs each, in order)")
for s in SIDES:
    print(f"-- {s}")
    for l in bench[s]:
        print("  ", l)
