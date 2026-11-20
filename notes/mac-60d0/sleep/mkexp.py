"""A copy of sleep-home's std with two probes (scratch only):
 - the reviewer's ICE: a sleep expires on a worker that is not its task's home;
 - counters of hand-overs asked, drops sent away and requests that waited for a carried sleep,
   printed with the rescue stats (VELT_RESCUE_STATS=1).
  python3 mkexp.py <out-dir>"""
import shutil
import sys

src = "/Users/andii/dev/velt/branches/sleep-home/std"
dst = sys.argv[1]
shutil.rmtree(dst, ignore_errors=True)
shutil.copytree(src, dst)


def edit(name, pairs):
    f = dst + "/lang/hooks/" + name
    s = open(f).read()
    for old, new in pairs:
        assert s.count(old) == 1, (name, old, s.count(old))
        s = s.replace(old, new)
    open(f, "w").write(s)


edit(
    "sched_sleep.velt",
    [
        (
            "      pushDue(k, f);\n",
            "      if (m.ld(k + t.T_HOME) !== t.exec()) {\n"
            '        panic("ICE: probe: exp: a sleep expires off its task\'s home");\n'
            "      }\n      pushDue(k, f);\n",
        )
    ],
)
edit(
    "sched_stats.velt",
    [
        (
            "const LOCKS_ON = new Atomic<u64>(0);\n",
            "const LOCKS_ON = new Atomic<u64>(0);\nconst ASKED = new Atomic<u64>(0);\nconst DROPPED = new Atomic<u64>(0);\nconst WAITED = new Atomic<u64>(0);\n"
            "export function probe(i: i64): void {\n  if (i === 0) {\n    ASKED.add(1);\n  } else if (i === 1) {\n    DROPPED.add(1);\n  } else {\n    WAITED.add(1);\n  }\n}\n",
        ),
        (
            "locks-on=${LOCKS_ON.load()}\\n`,",
            "locks-on=${LOCKS_ON.load()} asked=${ASKED.load()} dropped-away=${DROPPED.load()} waited=${WAITED.load()}\\n`,",
        ),
    ],
)
edit(
    "sched_sleep_move.velt",
    [
        ('import * as sl from "./sched_sleep";\n', 'import * as sl from "./sched_sleep";\nimport * as ps from "./sched_stats";\n'),
        ("  post(f, m.wakerClone(waker));\n", "  ps.probe(0);\n  post(f, m.wakerClone(waker));\n"),
        ("  post(f, 0);\n", "  ps.probe(1);\n  post(f, 0);\n"),
        ("    m.pushStack(e + t.E_SLEEP_ASKS, r, R_NEXT);\n    return;\n", "    ps.probe(2);\n    m.pushStack(e + t.E_SLEEP_ASKS, r, R_NEXT);\n    return;\n"),
    ],
)
print(dst)
