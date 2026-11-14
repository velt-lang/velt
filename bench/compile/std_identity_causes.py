#!/usr/bin/env python3
"""Why std definitions differ between programs (#2943 phase 0), by the fields that differ.

    bench/compile/std_identity_causes.py REFERENCE_DIR PROGRAM_DIR...

Each directory holds the `VELT_STD_SUMMARY_SHOW=std/` dump of one check: a file per std
definition with its hashed form, a field per line (`VELT_STD_SUMMARY=<dir>/d`). For every
definition whose file differs from the reference's, the names of the fields on the differing lines
(`assigned`, `mode`, `mutable`, `changes_held`, `callee`, ...) are counted: the field tells the
cause. A definition whose hashed form is equal but whose interface hash differed has its cause in
the program's tables, not in the HIR definition (`interface::hashing`'s facts); it isn't listed.
"""

import collections
import os
import re
import sys

FIELD = re.compile(r"([a-z_]+): ")


def fields(lines):
    """The field names a set of differing lines name; the last on each line is the innermost."""
    out = set()
    for line in lines:
        names = FIELD.findall(line)
        out.add(names[-1] if names else line.strip()[:40])
    return out


def main():
    ref_dir, programs = sys.argv[1], sys.argv[2:]
    causes = collections.Counter()  # field -> distinct definitions
    examples = collections.defaultdict(list)
    seen = set()
    for prog in programs:
        for name in sorted(os.listdir(prog)):
            ref = os.path.join(ref_dir, name)
            if name in seen or not os.path.exists(ref):
                continue
            a = open(ref).read().splitlines()
            b = open(os.path.join(prog, name)).read().splitlines()
            if a == b:
                continue
            seen.add(name)
            sa, sb = set(a), set(b)
            changed = [l for l in a if l not in sb] + [l for l in b if l not in sa]
            for f in fields(changed):
                causes[f] += 1
                if len(examples[f]) < 4:
                    examples[f].append(f"{name} ({os.path.basename(prog)})")
    for f, n in causes.most_common():
        print(f"{n:6} {f}")
        for e in examples[f]:
            print(f"         {e}")


if __name__ == "__main__":
    main()
