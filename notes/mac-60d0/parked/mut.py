"""Break checks for #3274 (parked-task) in a copy of its std.
  python3 mut.py <std-dir> gate|nested|joinparked|eager
gate: runEnd never finds a task parked for ever.
nested: an owner-only promise under an adoption keeps its awaiter (the outermost adopter's waker).
joinparked: dropping the last handle of a parked task does not wake it to find out."""
import sys

d = sys.argv[1] + "/lang/hooks/"
kind = sys.argv[2]
edits = {
    "gate": ("sched_run.velt", "  } else if (lt.parks(k)) {", "  } else if (false) {"),
    "nested": ("sched_node.velt", "  } else if (lost.nested(e, task, n, cx, fl)) {", "  } else if (false) {"),
    "keep": ("sched_node_lost.velt", "  const w = aw.takePlain(n);\n  if (w !== 0) {\n    m.wakerDrop(w);\n  }\n  if ((was & lt.LISTED) === 0) {", "  const w = 0;\n  if (w !== 0) {\n    m.wakerDrop(w);\n  }\n  if ((was & lt.LISTED) === 0) {"),
    "handout": ("sched_task_lost.velt", "export function list(task: usize, n: usize): void {\n", "export function list(task: usize, n: usize): void {\n  m.wakerClone(m.ld(n + nd.N_CX + 8));\n"),
    "handlate": ("sched_task_lost.velt", "  m.st(task + t.T_LISTED, m.ld(task + t.T_LISTED) + 1);\n}", "  m.st(task + t.T_LISTED, m.ld(task + t.T_LISTED) + 1);\n  m.wakerClone(m.ld(n + nd.N_CX + 8));\n}"),
    "eager": ("sched_node.velt", "const owned = r !== 1 && m.load(n + N_REFS) === 2;", "const owned = r !== 1 && m.load(n + N_REFS) >= 2;"),
    "joinparked": (
        "sched_join.velt",
        "  if ((s & (t.J_PARKED | t.J_DONE)) === t.J_PARKED) {",
        "  if (false) {",
    ),
}
f, old, new = edits[kind]
s = open(d + f).read()
assert s.count(old) == 1, (kind, old)
open(d + f, "w").write(s.replace(old, new))
