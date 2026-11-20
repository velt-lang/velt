"""A copy of sleep-home's std whose VELT_SCHED_DEBUG line also shows, per worker, whether it has
sleep requests waiting, pending sleeps, and sleeps on due lists; plus counters of the hand-over
(asked, served-new, served-fired, forwarded, waited, dropped).   python3 mkdbg.py <out-dir>"""
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
        assert s.count(old) == 1, (name, old[:60], s.count(old))
        s = s.replace(old, new)
    open(f, "w").write(s)


edit(
    "sched_debug.velt",
    [
        (
            " due=${(m.load(b + io.IO_DUE) | m.load(b + io.IO_RDUE)) === 0 ? 0 : 1}`;",
            " due=${(m.load(b + io.IO_DUE) | m.load(b + io.IO_RDUE)) === 0 ? 0 : 1} asks=${m.load(w + t.E_SLEEP_ASKS) === 0 ? 0 : 1} tm=${tmOf(w)} c=${C0.load()}/${C1.load()}/${C2.load()}/${C3.load()}/${C4.load()}/${C5.load()}`;",
        ),
        (
            "/** The milliseconds between two lines",
            "export const C0 = new Atomic<usize>(0);\nexport const C1 = new Atomic<usize>(0);\nexport const C2 = new Atomic<usize>(0);\nexport const C3 = new Atomic<usize>(0);\nexport const C4 = new Atomic<usize>(0);\nexport const C5 = new Atomic<usize>(0);\n\n"
            "function tmOf(w: usize): string {\n  const b = m.load(w + t.E_TIMERS);\n  if (b === 0) {\n    return \"-\";\n  }\n  return `${m.load(b) === 0 ? 0 : 1}p${m.load(b + 288)}d${m.load(b + 24)}u`;\n}\n\n"
            "/** The milliseconds between two lines",
        ),
    ],
)
edit(
    "sched_sleep_move.velt",
    [
        ('import * as m from "./sched_mem";\n', 'import * as dbg from "./sched_debug";\nimport * as m from "./sched_mem";\n'),
        ("  post(f, m.wakerClone(waker));\n", "  dbg.C0.add(1, \"relaxed\");\n  post(f, m.wakerClone(waker));\n"),
        ("  post(f, 0);\n", "  dbg.C5.add(1, \"relaxed\");\n  post(f, 0);\n"),
        ("    send(m.load(f + sl.S_HOME), r);\n    return;\n", "    dbg.C3.add(1, \"relaxed\");\n    send(m.load(f + sl.S_HOME), r);\n    return;\n"),
        ("    m.pushStack(e + t.E_SLEEP_ASKS, r, R_NEXT);\n    return;\n", "    dbg.C4.add(1, \"relaxed\");\n    m.pushStack(e + t.E_SLEEP_ASKS, r, R_NEXT);\n    return;\n"),
        ("  if (waker !== 0) {\n    m.wakeOwned(waker);\n  }\n", "  if (found === 2) {\n    dbg.C2.add(1, \"relaxed\");\n  } else {\n    dbg.C1.add(1, \"relaxed\");\n  }\n  if (waker !== 0) {\n    m.wakeOwned(waker);\n  }\n"),
    ],
)
print(dst)
