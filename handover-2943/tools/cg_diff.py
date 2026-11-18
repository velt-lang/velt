# cg_diff.py <inclusive-a.txt> <inclusive-b.txt> [N] [filter]: callgrind_annotate --inclusive
# outputs of two runs compared by function: each velt function's inclusive instructions in a and
# in b, sorted by b - a (largest first). A function is named by its symbol (crate path kept,
# the `'2` recursion suffix and the file dropped); `filter` keeps names containing it.
import re
import sys

LINE = re.compile(r"^\s*([\d,]+) \(\s*[\d.]+%\)\s+(?:\*\s+)?(?:[^:]*:)?(.+?)(?: \[[^\]]*\])?$")


def load(path):
    out = {}
    for line in open(path, errors="replace"):
        if "=>" in line or "<" in line[:30]:
            continue
        m = LINE.match(line)
        if not m:
            continue
        n = int(m.group(1).replace(",", ""))
        name = re.sub(r"'\d+$", "", m.group(2).strip())
        if not re.search(r"velt|vpm", name):
            continue
        out[name] = max(out.get(name, 0), n)
    return out


a, b = load(sys.argv[1]), load(sys.argv[2])
top = int(sys.argv[3]) if len(sys.argv) > 3 else 40
want = sys.argv[4] if len(sys.argv) > 4 else ""
rows = []
for name in set(a) | set(b):
    if want and want not in name:
        continue
    rows.append((b.get(name, 0) - a.get(name, 0), a.get(name, 0), b.get(name, 0), name))
rows.sort(reverse=True)
print(f"{'b - a':>12} {'a':>12} {'b':>12}  function")
for d, x, y, name in rows[:top]:
    print(f"{d / 1e6:11.1f}M {x / 1e6:11.1f}M {y / 1e6:11.1f}M  {name[:150]}")
