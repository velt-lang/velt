"""#3369 review item 1, first try: the transferred value reaches the outcome by move (through a
one-element array, as takeSettlement moves it out).  python3 movefix.py <settle.velt>"""
import sys

p = sys.argv[1]
s = open(p).read()
for old, new in (
    (
        "  const v = transfer(value);\n"
        '  return guard.with((g) => slot.with((s) => s.settle({ status: "fulfilled", value: v })));\n',
        "  // In an array, so the closures below move the outcome out (`pop`) instead of copying what\n"
        "  // they captured: a copy of a promise shares it on this task.\n"
        '  const out: Settlement<T, E>[] = [{ status: "fulfilled", value: transfer(value) }];\n'
        '  return guard.with((g) => slot.with((s) => s.settle(out.pop() ?? { status: "abandoned" })));\n',
    ),
    (
        "  const r = transfer(reason);\n"
        '  return guard.with((g) => slot.with((s) => s.settle({ status: "rejected", reason: r })));\n',
        '  const out: Settlement<T, E>[] = [{ status: "rejected", reason: transfer(reason) }];\n'
        '  return guard.with((g) => slot.with((s) => s.settle(out.pop() ?? { status: "abandoned" })));\n',
    ),
):
    assert s.count(old) == 1, old[:50]
    s = s.replace(old, new)
open(p, "w").write(s)
