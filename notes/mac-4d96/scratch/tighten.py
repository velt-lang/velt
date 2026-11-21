"""#3273 owner fix, two tightenings: one compare for the own-waker test, and the registration
anchors with the worker it already has.  python3 tighten.py <std>"""
import sys

root = sys.argv[1] + "/lang/hooks/"


def edit(name, pairs):
    p = root + name
    s = open(p).read()
    for old, new in pairs:
        assert s.count(old) == 1, (name, old[:70], s.count(old))
        s = s.replace(old, new)
    open(p, "w").write(s)


edit(
    "sched_sleep.velt",
    [
        (
            "    const own = s === NEW && (w === cx - t.T_CX || w === cx - nd.N_CX);\n",
            "    // One compare for both: the two offsets differ in one bit (`OWN_BIT`).\n"
            "    const own = s === NEW && ((cx - w) | OWN_BIT) === t.T_CX;\n",
        ),
        (
            "    sp.anchor();\n    const e = t.exec();\n    // Unordered: `insert` stores the state after it, released.\n",
            "    const e = t.exec();\n    sp.anchorOn(e);\n    // Unordered: `insert` stores the state after it, released.\n",
        ),
        (
            "export const S_SIZE: usize = 96;\n",
            "export const S_SIZE: usize = 96;\n\n"
            "/**\n"
            " * The bit in which the place of a task's own context in the task (`T_CX`) differs from the place\n"
            " * of a promise's in its node (`N_CX`): with it set, either offset is `T_CX` (`sleepPoll`).\n"
            " */\n"
            "const OWN_BIT: usize = t.T_CX ^ nd.N_CX;\n",
        ),
    ],
)
edit(
    "sched_spread.velt",
    [
        (
            "/**\n * As `anchor`, for good (`FIXED`)",
            "/** `anchor` on worker `e`, which the caller has at hand (sched_sleep.velt `sleepPoll`). */\n"
            "@inline(\"always\")\n"
            "export function anchorOn(e: usize): void {\n"
            "  const k = m.ld(e + t.E_CURRENT);\n"
            "  if (k !== 0 && !t.has(k, t.ANCHORED)) {\n"
            "    anchorTo(e, k);\n"
            "  }\n"
            "}\n\n"
            "/**\n * As `anchor`, for good (`FIXED`)",
        )
    ],
)
