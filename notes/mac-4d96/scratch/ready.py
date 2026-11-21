"""t_two.velt: the droppers are counted in instead of a sleep.  python3 ready.py <file>"""
import sys

p = sys.argv[1]
s = open(p).read()
for old, new in (
    ("const DONE = new Atomic<i64>(0);\n", "const READY = new Atomic<i64>(0);\nconst DONE = new Atomic<i64>(0);\n"),
    ("  while (GO.load() <= r) {}\n", "  READY.add(1);\n  while (GO.load() <= r) {}\n"),
    (
        "    await sleep(1);\n    GO.store(r + 1);\n",
        "    // Both droppers spin before they are released, so they drop at the same moment.\n"
        "    while (READY.load() < 2 * (r + 1)) {\n      await yieldNow();\n    }\n    GO.store(r + 1);\n",
    ),
    ("// same moment (they spin on one flag)", "// same moment (both spin on one flag, released once both are spinning)"),
):
    assert s.count(old) == 1, old
    s = s.replace(old, new)
open(p, "w").write(s)
