#!/usr/bin/env python3
"""The std identity oracle's report (#2943 phase 0).

    bench/compile/std_identity.py REFERENCE.tsv PROGRAM.tsv...

REFERENCE is what `VELT_STD_SUMMARY=REFERENCE.tsv velt check --std` wrote; each PROGRAM file what
a check of one program wrote (bench/compile/std_identity.sh makes both). A line is
`<name>\t<interface hash>\t<whole hash>`, after a `# <root file>` line.

A std definition *differs* in a program when its checked form there isn't the one `check --std`
gives it: separate compilation (#2943) needs every std definition the same in every program.
Prints how many programs and definitions differ, whether the interface (what callers see) or
only the rest (the body) differs, the definitions that differ in the most programs, and std
definitions a program has that `check --std` doesn't (made per program). Exit status 1 when any
definition differs, so CI can require none.
"""

import collections
import sys


def read(path):
    """{root: {name: (interface, whole)}} of one summary file."""
    out = {}
    root = None
    with open(path) as f:
        for line in f:
            line = line.rstrip("\n")
            if line.startswith("# "):
                root = line[2:]
                # A source generator's host program is checked on the side: not a program.
                if root.endswith("__velt_generate_host.velt"):
                    root = None
                    continue
                out[root] = {}
            elif line and root is not None:
                name, interface, whole = line.split("\t")
                # A name may repeat (two definitions with one display name): keep all.
                out[root].setdefault(name, []).append((interface, whole))
    return out


def main():
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    ref_all = read(sys.argv[1])
    if len(ref_all) != 1:
        print(f"expected one check in {sys.argv[1]}, found {len(ref_all)}", file=sys.stderr)
        return 2
    ref = next(iter(ref_all.values()))
    programs = 0
    differing_programs = 0
    by_def = collections.Counter()  # name -> programs where it differs
    iface = collections.Counter()  # name -> programs where its interface differs
    extra = collections.Counter()  # name -> programs that have it, check --std doesn't
    compared = 0
    differing = 0
    example = {}
    for path in sys.argv[2:]:
        for root, defs in read(path).items():
            programs += 1
            any_diff = False
            for name, hashes in defs.items():
                mine = ref.get(name)
                if mine is None:
                    extra[name] += 1
                    continue
                compared += 1
                if sorted(hashes) != sorted(mine):
                    differing += 1
                    any_diff = True
                    by_def[name] += 1
                    example.setdefault(name, root)
                    if sorted(h[0] for h in hashes) != sorted(h[0] for h in mine):
                        iface[name] += 1
            differing_programs += any_diff
    print(f"programs: {programs}, differing: {differing_programs}")
    print(f"std definitions compared: {compared}, differing: {differing} "
          f"({len(by_def)} distinct, {len(iface)} of them in their interface)")
    print(f"std definitions only in programs: {len(extra)} distinct")
    print("\nmost differing (programs, interface?, name, first program):")
    for name, n in by_def.most_common(60):
        print(f"{n:6} {'iface' if name in iface else 'body ':5} {name}  ({example[name]})")
    if extra:
        print("\nonly in programs (programs, name):")
        for name, n in extra.most_common(20):
            print(f"{n:6} {name}")
    return 1 if differing else 0


if __name__ == "__main__":
    sys.exit(main())
