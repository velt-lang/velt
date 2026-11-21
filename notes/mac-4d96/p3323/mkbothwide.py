"""Scratch stds on the #3323 prototype with the settling step's window widened (0.3 ms spin
before fulfillLater's frame ends): `std-proto-wide` (main's settle.velt) and `std-both-wide`
(#3360's fixed settle.velt).  python3 mkbothwide.py <dir>"""
import shutil
import sys

B = "/Users/andii/dev/velt/branches"
out = sys.argv[1]
spin = "  const until = performance.now() + 0.3;\n  while (performance.now() < until) {}\n}\n"
for name, settle, tail in (
    ("proto-wide", None, '  openLatch(guard.with((g) => slot.with((s) => s.settle({ status: "fulfilled", value: v }))));\n}\n'),
    ("both-wide", B + "/settle-race/std/lang/promise/settle.velt", "  openLatch(fulfillFrom<T, E>(slot, guard, value));\n}\n"),
):
    dst = f"{out}/std-{name}"
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(B + "/parked-away/std", dst)
    p = dst + "/lang/promise/settle.velt"
    if settle:
        shutil.copyfile(settle, p)
    s = open(p).read()
    assert s.count(tail) == 1, name
    open(p, "w").write(s.replace(tail, tail[:-2] + spin))
    print(dst)
