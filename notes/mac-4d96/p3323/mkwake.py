"""nodeWake's window widened (a pause between the wake and the waker's release), with item 1
(`wake-fix`) and without it (`wake-noref`).  python3 mkwake.py <out-dir>"""
import shutil
import sys

src = "/Users/andii/dev/velt/branches/parked-away/std"
out = sys.argv[1]
old = "  wake(w);\n  nd.release(w);\n}\n\n/** `nodeWake` with the block counters on"
new = "  wake(w);\n  for (let i = 0; i < 3000; i++) {\n    m.yieldCore();\n  }\n  nd.release(w);\n}\n\n/** `nodeWake` with the block counters on"
old2 = "  wakingStarts();\n  wake(w);\n  nd.release(w);\n"
for name in ("wake-fix", "wake-noref"):
    dst = f"{out}/std-{name}"
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst)
    p = dst + "/lang/hooks/sched_node_wake.velt"
    s = open(p).read()
    assert s.count(old) == 1, name
    s = s.replace(old, new)
    # The counted path (the tests' runs) the same way.
    o2 = "  wk.wakingStarts();\n  wake(w);\n  nd.release(w);\n"
    assert s.count(o2) == 1, (name, "counted")
    s = s.replace(o2, "  wk.wakingStarts();\n  wake(w);\n  for (let i = 0; i < 3000; i++) {\n    m.yieldCore();\n  }\n  nd.release(w);\n")
    open(p, "w").write(s)
    if name == "wake-noref":
        p = dst + "/lang/hooks/sched_node.velt"
        s = open(p).read()
        assert s.count("      lost.taskRef(task);\n") == 1
        open(p, "w").write(s.replace("      lost.taskRef(task);\n", "      wk.release(task);\n"))
    print(dst)
