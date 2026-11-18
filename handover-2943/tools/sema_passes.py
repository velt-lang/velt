# sema_passes.py <dir>: the "velt-sema: totals" sections of <dir>/{lazy,units}-N.txt (sema-passes.sh)
# as medians per pass and mode, side by side, largest units - lazy first. Passes under 1 ms in a
# run are not printed by velt: missing counts as 0.
import glob
import re
import statistics
import sys

d = sys.argv[1]
LINE = re.compile(r"^velt-sema:\s+(\S.*?)\s+([\d.]+) ms\s+x\d+$")


def totals(path):
    out, on = {}, False
    for line in open(path, errors="replace"):
        if line.startswith("velt-sema: totals"):
            on = True
            continue
        if on:
            m = LINE.match(line.rstrip())
            if m:
                out[m.group(1)] = float(m.group(2))
    return out


modes = {}
for mode in ("lazy", "units"):
    runs = [totals(p) for p in sorted(glob.glob(f"{d}/{mode}-*.txt"))]
    names = set().union(*runs) if runs else set()
    modes[mode] = {n: statistics.median(r.get(n, 0.0) for r in runs) for n in names}
names = set(modes["lazy"]) | set(modes["units"])
rows = sorted(
    ((modes["units"].get(n, 0) - modes["lazy"].get(n, 0), n) for n in names), reverse=True
)
print(f"{'units-lazy':>10} {'lazy':>8} {'units':>8}  pass (ms, median of runs)")
for diff, n in rows[:30]:
    print(f"{diff:10.1f} {modes['lazy'].get(n, 0):8.1f} {modes['units'].get(n, 0):8.1f}  {n}")
print(f"{'':>10} {sum(modes['lazy'].values()):8.1f} {sum(modes['units'].values()):8.1f}  sum of passes")
