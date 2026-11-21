"""A copy of a std tree with counters (scratch only), printed with the rescue stats
(VELT_RESCUE_STATS=1): hand-overs asked, and what `release` found a requested sleep as:
pending, due, fired, or on no list.   python3 mkcnt.py <src-std> <out-dir>"""
import shutil
import sys

src, dst = sys.argv[1], sys.argv[2]
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
    "sched_stats.velt",
    [
        (
            "const LOCKS_ON = new Atomic<u64>(0);\n",
            "const LOCKS_ON = new Atomic<u64>(0);\nconst P0 = new Atomic<u64>(0);\nconst P1 = new Atomic<u64>(0);\nconst P2 = new Atomic<u64>(0);\nconst P3 = new Atomic<u64>(0);\nconst P4 = new Atomic<u64>(0);\nconst P5 = new Atomic<u64>(0);\nconst P6 = new Atomic<u64>(0);\nconst P7 = new Atomic<u64>(0);\n"
            "export function probe(i: i64): void {\n  if (i === 0) {\n    P0.add(1);\n  } else if (i === 1) {\n    P1.add(1);\n  } else if (i === 2) {\n    P2.add(1);\n  } else if (i === 3) {\n    P3.add(1);\n  } else if (i === 4) {\n    P4.add(1);\n  } else if (i === 5) {\n    P5.add(1);\n  } else if (i === 6) {\n    P6.add(1);\n  } else {\n    P7.add(1);\n  }\n}\n",
        ),
        (
            "locks-on=${LOCKS_ON.load()}\\n`,",
            "locks-on=${LOCKS_ON.load()} asked= ${P0.load()} rel-pending= ${P1.load()} rel-due= ${P2.load()} rel-fired= ${P3.load()} rel-none= ${P4.load()} poll-pending= ${P5.load()} poll-due= ${P6.load()} poll-fired= ${P7.load()}\\n`,",
        ),
    ],
)
edit(
    "sched_sleep_move.velt",
    [
        ('import * as sl from "./sched_sleep";\n', 'import * as sl from "./sched_sleep";\nimport * as ps from "./sched_stats";\n'),
        ("  UNDER_WAY.add(1);\n", "  ps.probe(0);\n  UNDER_WAY.add(1);\n"),
        ("  if (s === sl.PENDING) {\n    sl.unlinkPending(sl.block(e), f);\n  } else if (s === sl.DUE) {\n    sl.unlinkDue(m.ld(f + sl.S_TASK), f);\n  } else if (s === sl.FIRED) {\n    sl.seenOn(e, f);\n  } else if (!drop) {",
         "  if (s === sl.PENDING) {\n    ps.probe(1);\n    sl.unlinkPending(sl.block(e), f);\n  } else if (s === sl.DUE) {\n    ps.probe(2);\n    sl.unlinkDue(m.ld(f + sl.S_TASK), f);\n  } else if (s === sl.FIRED) {\n    ps.probe(3);\n    sl.seenOn(e, f);\n  } else if (!drop) {\n    ps.probe(4);"),
    ],
)
print(dst)
edit(
    "sched_sleep.velt",
    [
        ('import * as mv from "./sched_sleep_move";\n', 'import * as mv from "./sched_sleep_move";\nimport * as ps from "./sched_stats";\n'),
        ("    // Polled before. The task that polled it into its block", "    ps.probe(s === PENDING ? 5 : s === DUE ? 6 : 7);\n    // Polled before. The task that polled it into its block"),
    ],
)
