"""#3273, on top of noowner.py: the owner word comes back for pending sleeps, set only by a
first registration through a waker that keeps the polling task alive.  python3 pinowner.py <std>"""
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
            "export const S_TASK: usize = 64;\n",
            "export const S_TASK: usize = 64;\n"
            "/**\n"
            " * While it is pending: the await word (`T_WORD`) of the task that polled it into its worker's\n"
            " * timer block, what that task's contexts point at, if the waker it was registered with keeps\n"
            " * that task alive (`sleepPoll`); else 0. A word that is not 0 names a live task, never an\n"
            " * address a task made later was given.\n"
            " */\n"
            "export const S_OWNER: usize = 72;\n",
        ),
        (
            "    } else if (!here(f, s)) {\n"
            "      // Pending in another worker's block, or carried by an offered task: its worker gives it\n"
            "      // up, and this task polls it again as a new one. A pending sleep keeps no word of the\n"
            "      // task that polled it there: nothing would keep that task alive (its waker may be a\n"
            "      // combinator's), and a task made later at its address would pass for it.\n"
            "      return mv.ask(f, m.ld(cx + 8));\n"
            "    }\n",
            "    } else if (m.ld(cx) !== m.loadRelaxed(f + S_OWNER) && !here(f, s)) {\n"
            "      // Pending in another worker's block, or carried by an offered task: its worker gives it\n"
            "      // up, and this task polls it again as a new one. The shortcut: a poll whose context\n"
            "      // names the task the sleep keeps as its owner (`S_OWNER`, a live task: below) is that\n"
            "      // task's, which is anchored to the worker that holds the sleep.\n"
            "      return mv.ask(f, m.ld(cx + 8));\n"
            "    }\n",
        ),
        (
            "  if (old !== w) {\n    m.st(f + S_WAKER, m.wakerClone(w));\n",
            "  if (old !== w) {\n"
            "    // The owner word, an address, may only name a task that cannot be freed while the sleep\n"
            "    // names it, or a task made later at that address would pass for the owner on whatever\n"
            "    // worker it runs (#3207). What keeps the polling task is the sleep's own waker when it\n"
            "    // is that task's or a promise's of it, the waker of the context itself then (a context\n"
            "    // sits inside the object its waker is: `T_CX`, `N_CX`): a clone of it is a reference. A\n"
            "    // combinator's waker keeps no task, and neither does a waker registered later. Stored\n"
            "    // before the clone's call, which then needs no context: the task is the one polling.\n"
            "    const own = s === NEW && (w === cx - t.T_CX || w === cx - nd.N_CX);\n"
            "    m.storeRelaxed(f + S_OWNER, own ? m.ld(cx) : 0);\n"
            "    m.st(f + S_WAKER, m.wakerClone(w));\n",
        ),
        ('import * as m from "./sched_mem";\n', 'import * as m from "./sched_mem";\nimport * as nd from "./sched_node";\n'),
    ],
)
edit(
    "sched_sleep_carry.velt",
    [
        (
            "    // Unordered: `insert` stores the state after it, released.\n    m.storeRelaxed(f + sl.S_HOME, e);\n",
            "    // No owner word: the one it had named the task that polled it first (sched_sleep.velt).\n"
            "    m.storeRelaxed(f + sl.S_OWNER, 0);\n"
            "    // Unordered: `insert` stores the state after it, released.\n    m.storeRelaxed(f + sl.S_HOME, e);\n",
        )
    ],
)
