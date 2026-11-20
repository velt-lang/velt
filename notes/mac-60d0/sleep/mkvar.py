"""Measurement-only variants of sleep-home's std (never committed): each takes one piece of the
#3273 cost out, so cachegrind shows what each piece costs per row.
  python3 mkvar.py <out-dir>   -> <out-dir>/<variant>/std for each variant"""
import os
import shutil
import sys

SRC = "/Users/andii/dev/velt/branches/sleep-home/std"
out = sys.argv[1]
F = "lang/hooks/sched_sleep.velt"

VARIANTS = {
    "noasks": [
        (
            "  if (m.load(f + S_ASKS) !== 0) {\n    // A hand-over under way names it: the worker that one reaches frees it.\n",
            "  if (false) {\n",
        )
    ],
    "nocheck": [("    if (m.ld(cx) - t.T_WORD !== m.ld(f + S_OWNER) && !here(f, s)) {", "    if (false) {")],
    "nohome": [("    m.storeRelease(f + S_HOME, e);\n    insert(block(e), f);", "    insert(block(e), f);")],
    "noexpchk": [("      if (check && m.ld(k + t.T_HOME) !== t.exec()) {", "      if (false) {")],
    "nodropchk": [
        (
            "    if (s === CARRIED || m.load(f + S_HOME) !== e) {\n      // Its worker takes it off",
            "    if (false) {\n      // Its worker takes it off",
        ),
        ("    if (wk.counting() && m.ld(f + S_HOME) !== e) {\n      held(\"is dropped\");", "    if (false) {\n      held(\"is dropped\");"),
    ],
    "plain": [],
}

for name, edits in VARIANTS.items():
    d = os.path.join(out, name, "std")
    if os.path.exists(d):
        shutil.rmtree(d)
    shutil.copytree(SRC, d)
    p = os.path.join(d, F)
    s = open(p).read()
    for old, new in edits:
        assert s.count(old) == 1, (name, old)
        s = s.replace(old, new)
    if name == "plain":
        n = s.count("m.storeRelease(")
        s = s.replace("m.storeRelease(", "m.st(")
        print("plain: replaced", n)
    open(p, "w").write(s)
    print("variant", name)
