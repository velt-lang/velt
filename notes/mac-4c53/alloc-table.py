import re
import sys

rows = []
for line in open(sys.argv[1]):
    if not line.startswith("bench_"):
        continue
    parts = line.split()
    name = parts[0][len("bench_"):].replace("json_", "json/")
    vals = {}
    for p in parts[1:]:
        m = re.match(r"(base|head|noattr|inline)1=(\d+)", p)
        if m:
            vals[m.group(1)] = int(m.group(2))
    rows.append((name, vals))


def pct(a, b):
    return (b - a) / a * 100


print("| Row | base | head | head vs base | no attributes vs base | inlinable vs base |")
print("|---|---:|---:|---:|---:|---:|")
for name, v in rows:
    b = v["base"]
    print(
        f"| {name} | {b:,} | {v['head']:,} | {pct(b, v['head']):+.2f}% "
        f"| {pct(b, v['noattr']):+.2f}% | {pct(b, v['inline']):+.2f}% |"
    )
worst = max(rows, key=lambda r: pct(r[1]["base"], r[1]["head"]))
best = min(rows, key=lambda r: pct(r[1]["base"], r[1]["head"]))
print(f"\nhead: worst {worst[0]} {pct(worst[1]['base'], worst[1]['head']):+.3f}%, best {best[0]} {pct(best[1]['base'], best[1]['head']):+.3f}%")
for side in ("noattr", "inline"):
    big = [(n, pct(v["head"], v[side])) for n, v in rows if abs(pct(v["head"], v[side])) > 0.3]
    print(side, "vs head beyond 0.3%:", ", ".join(f"{n} {d:+.2f}%" for n, d in big))
