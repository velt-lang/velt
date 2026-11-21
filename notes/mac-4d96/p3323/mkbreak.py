"""Broken copies of parked-away's std for #3323's break checks (scratch only):
  nogive:  the other-side drop releases as before (no hand-over to the task)
  nowake:  the pin on the task is given back without the wake
  nowait:  a task is freed while its join lock is held
python3 mkbreak.py <out-dir>"""
import shutil
import sys

src = "/Users/andii/dev/velt/branches/parked-away/std"
out = sys.argv[1]
EDITS = {
    "nogive": ("sched_node_lost.velt", "  if (!lone || task === 0 || lk.mode() !== lk.LK_ON) {", "  if (true) {"),
    "nowake": ("sched_task_lost.velt", "  if (!last) {\n    wk.wake(k);\n  }\n", ""),
    "nowait": ("sched_wake.velt", "      awaitUnlocked(t);\n", "      panic(\"ICE: probe: a task is freed while its join lock is held\");\n"),
}
for name, (f, old, new) in EDITS.items():
    dst = f"{out}/std-{name}"
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst)
    p = dst + "/lang/hooks/" + f
    s = open(p).read()
    assert s.count(old) == 1, (name, s.count(old))
    open(p, "w").write(s.replace(old, new))
    print(dst)
