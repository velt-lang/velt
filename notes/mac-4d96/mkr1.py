"""Std variants for #3273's R1 (spawn_many at T4), from the sleep-home worktree's std (the PR
without #3322):
  pr       the PR as pushed
  nowake   the offer take-back does not wake a sleeping helper a second time (measurement only)
  notick   the I/O tick does not look at the request list or at its own offer (measurement only)
python3 mkr1.py"""
import shutil

B = "/Users/andii/dev/velt/branches"
VAR = B + "/sleep-combo-var"
EDITS = {
    "pr": [],
    "nowake": [
        (
            "sched_spread.velt",
            "  const h = sleepingHelper(e);\n  if (h !== 0) {\n    wk.unpark(h);\n    return;\n  }\n  steal(e);\n",
            "  if (sleepingHelper(e) !== 0) {\n    return;\n  }\n  steal(e);\n",
        )
    ],
    "notick": [
        (
            "sched_tick.velt",
            "  if (m.load(e + t.E_SLEEP_ASKS) !== 0) {\n    mv.serve(e);\n  }\n  const offered = m.load(e + t.E_OFFER);\n  if (offered !== 0) {\n",
            "  const offered: usize = 0;\n  if (offered !== 0) {\n",
        )
    ],
}
for name, edits in EDITS.items():
    dst = f"{VAR}/r1-{name}"
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(B + "/sleep-home/std", dst + "/std")
    for f, old, new in edits:
        p = dst + "/std/lang/hooks/" + f
        s = open(p).read()
        assert s.count(old) == 1, (name, f, s.count(old))
        open(p, "w").write(s.replace(old, new))
    print(dst)
