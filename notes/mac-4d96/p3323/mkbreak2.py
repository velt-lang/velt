"""Broken copies of parked-away's working std for the review-round break checks:
  noref:   a freed node gives its task reference back with no wake (wk.release), as before
  oldlone: droppedAway's old test, with its window between the two loads widened
  newlone: the new test with the same widened window (the control)
python3 mkbreak2.py <out-dir>"""
import shutil
import sys

src = "/Users/andii/dev/velt/branches/parked-away/std"
out = sys.argv[1]
wide = "  for (let i = 0; i < 3000; i++) {\n    m.yieldCore();\n  }\n"
loads = "  const fl = m.load32(n + nd.N_FLAGS);\n  const refs = m.load(n + nd.N_REFS);\n"
newlone = "  const lone = refs === 1 || (refs === 2 && (fl & nd.FINISHED) === 0);\n"
oldlone = "  const lone = (fl & nd.FINISHED) !== 0 ? refs === 1 : refs === 2;\n"
EDITS = {
    "noref": [("sched_node.velt", "      lost.taskRef(task);\n", "      wk.release(task);\n")],
    "oldlone": [
        ("sched_node_lost.velt", loads, "  const fl = m.load32(n + nd.N_FLAGS);\n" + wide + "  const refs = m.load(n + nd.N_REFS);\n"),
        ("sched_node_lost.velt", newlone, oldlone),
    ],
    "newlone": [("sched_node_lost.velt", loads, "  const fl = m.load32(n + nd.N_FLAGS);\n" + wide + "  const refs = m.load(n + nd.N_REFS);\n")],
}
for name, edits in EDITS.items():
    dst = f"{out}/std-{name}"
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst)
    for f, old, new in edits:
        p = dst + "/lang/hooks/" + f
        s = open(p).read()
        assert s.count(old) == 1, (name, f, s.count(old))
        open(p, "w").write(s.replace(old, new))
    print(dst)
