#!/usr/bin/env python3
"""yard_diff.py A.md B.md: two yardstick reports (`yardstick.py --md`) side by side, per row of
its "Instructions per stage" and "The code and the process" tables: A -> B and the change in
percent. Columns empty in either report (release 100k measures only its IR lines) are left out
unless some row has them. Used to say which commit moved a row of the nightly's yardstick."""
import re
import sys


def tables(path):
    """{table title: (columns, {row: [value or None]})} of a yardstick report."""
    out, title, cols, rows = {}, None, [], {}
    for line in open(path, encoding="utf-8"):
        if line.startswith("####"):
            if title:
                out[title] = (cols, rows)
            title, cols, rows = line.strip("# \n"), [], {}
            continue
        if not title or not line.startswith("|") or line.startswith("|---"):
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if cells[0] in ("stage", "") and not cols:
            cols = cells[1:]
            continue
        vals = []
        for c in cells[1:]:
            m = re.match(r"\**([\d,]+(?:\.\d+)?)", c)
            vals.append(float(m.group(1).replace(",", "")) if m else None)
        rows[cells[0].strip("*")] = vals
    if title:
        out[title] = (cols, rows)
    return out


def num(x):
    return f"{x:,.0f}" if x == int(x) else f"{x:,.1f}"


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    a, b = tables(sys.argv[1]), tables(sys.argv[2])
    for title in a:
        if not (title.startswith("Instructions") or title.startswith("The code")):
            continue
        cols, ra = a[title]
        _, rb = b.get(title, (cols, {}))
        use = [i for i in range(len(cols))
               if any(r[i] is not None for r in ra.values() if i < len(r))]
        print(f"#### {title}\n")
        print("| | " + " | ".join(cols[i] for i in use) + " |")
        print("|---|" + "---:|" * len(use))
        for row, va in ra.items():
            vb = rb.get(row, [None] * len(cols))
            cells = []
            for i in use:
                x = va[i] if i < len(va) else None
                y = vb[i] if i < len(vb) else None
                if x is None or y is None:
                    cells.append("")
                    continue
                pct = f" ({(y - x) / x * 100:+.1f}%)" if x else ""
                cells.append(f"{num(x)} → {num(y)}{pct}")
            print(f"| {row} | " + " | ".join(cells) + " |")
        print()


if __name__ == "__main__":
    main()
