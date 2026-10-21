#!/usr/bin/env python3
"""Groups the errors two sigx runs differ by (standard library only, Python 3.8+).

  group_errors.py <base.json> <head.json> [--show N]

Each input is the JSON that count_errors.py writes (a list of {file, line, col, msg}), or a
list of [file, line, col, msg, notes] rows. Errors are counted by place (file, line and
column), as count_errors.py does: a module's error is reported again by every file importing
it, sometimes with other type texts. The script prints the places only the head reports, the
places only the base reports, and groups each by message shape: every backquoted text becomes
`_` and numbers become N, so "cannot cast `A` as `B`" groups with every other cast. Places both
runs report with different message shapes are listed as changed.
"""

import json
import re
import sys
from collections import defaultdict


def load(path):
    rows = json.load(open(path, encoding="utf-8"))
    out = defaultdict(set)
    for r in rows:
        if isinstance(r, dict):
            f, line, col, msg = r["file"], r["line"], r["col"], r["msg"]
        else:
            f, line, col, msg = r[0], r[1], r[2], r[3]
        f = re.sub(r"^.*?/(packages/)", r"\1", f.replace("\\", "/"))
        out[(f, line, col)].add(msg)
    return out


def shape(msg):
    s = re.sub(r"`[^`]*`", "`_`", msg)
    return re.sub(r"\b\d+\b", "N", s)


def report(title, places, runs, show):
    print(f"== {title}: {len(places)}")
    groups = defaultdict(list)
    for p in sorted(places):
        groups[" / ".join(sorted({shape(m) for m in runs[p]}))].append(p)
    for sh, ps in sorted(groups.items(), key=lambda kv: (-len(kv[1]), kv[0])):
        print(f"{len(ps):5}  {sh}")
        for p in ps[:show]:
            print(f"         {p[0]}:{p[1]}:{p[2]}: {sorted(runs[p])[0][:240]}")


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    show = 0
    if "--show" in sys.argv:
        show = int(sys.argv[sys.argv.index("--show") + 1])
        args = [a for a in args if a != str(show)]
    if len(args) != 2:
        sys.exit(__doc__)
    base, head = load(args[0]), load(args[1])
    print(f"base {len(base)} places, head {len(head)} places")
    report("only in head", [p for p in head if p not in base], head, show)
    report("only in base", [p for p in base if p not in head], base, show)
    changed = [
        p
        for p in head
        if p in base and {shape(m) for m in head[p]} != {shape(m) for m in base[p]}
    ]
    print(f"== changed message at the same place: {len(changed)}")
    for p in sorted(changed)[: max(show, 0) * 4]:
        print(f"         {p[0]}:{p[1]}:{p[2]}:")
        print(f"           base: {sorted(base[p])[0][:200]}")
        print(f"           head: {sorted(head[p])[0][:200]}")


if __name__ == "__main__":
    main()
