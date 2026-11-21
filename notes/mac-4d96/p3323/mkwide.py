"""Scratch stds that widen releaseAway's window (the dropper waits ~300 us between the count going
down and its wake + unlock), with the free's wait for the lock (`wide`) and without (`wide-nowait`):
python3 mkwide.py <out-dir>"""
import shutil
import sys

src = "/Users/andii/dev/velt/branches/parked-away/std"
out = sys.argv[1]
for name in ("wide", "wide-nowait"):
    dst = f"{out}/std-{name}"
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst)
    p = dst + "/lang/hooks/sched_task_lost.velt"
    s = open(p).read()
    old = "  const last = m.dec(k + t.T_REFS) === 1;\n  if (!last) {\n    wk.wake(k);\n"
    assert s.count(old) == 1
    s = s.replace(
        old,
        "  const last = m.dec(k + t.T_REFS) === 1;\n"
        "  if (!last) {\n"
        "    for (let i = 0; i < 20000; i++) {\n      m.yieldCore();\n    }\n"
        "    wk.wake(k);\n",
    )
    open(p, "w").write(s)
    if name == "wide-nowait":
        p = dst + "/lang/hooks/sched_wake.velt"
        s = open(p).read()
        old = "      awaitUnlocked(t);\n"
        assert s.count(old) == 1
        open(p, "w").write(s.replace(old, ""))
    print(dst)
