"""#3273 owner fix, follow-ups: the inline import, carry's module docs. python3 fix2.py <std>"""
import sys

root = sys.argv[1] + "/lang/hooks/"


def edit(name, pairs):
    p = root + name
    s = open(p).read()
    for old, new in pairs:
        assert s.count(old) == 1, (name, old[:70], s.count(old))
        s = s.replace(old, new)
    open(p, "w").write(s)


edit("sched_sleep_move.velt", [('import { noInline } from "velt/lang/intrinsics";\n', 'import { inline, noInline } from "velt/lang/intrinsics";\n')])
edit(
    "sched_sleep_carry.velt",
    [
        (
            "// - `carry`, on the offering worker: the pending sleeps of the offered tasks (the task that polled\n"
            "//   one, `S_OWNER`, or the one its waker wakes) leave the block, in one walk, for a list in their\n"
            "//   task's `T_TIMERS`, in timer order, and such a task is marked `CARRIED`.\n",
            "// - `carry`, on the offering worker: the pending sleeps of the offered tasks (the task a sleep's\n"
            "//   waker wakes; one whose waker is not a task's stays, `owner`) leave the block, in one walk,\n"
            "//   for a list in their task's `T_TIMERS`, in timer order, and such a task is marked `CARRIED`.\n",
        )
    ],
)
