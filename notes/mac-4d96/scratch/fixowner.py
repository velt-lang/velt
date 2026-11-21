"""#3273: the owner word only while the registered waker keeps the owner alive.
python3 fixowner.py <std>"""
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
        # The waker's store decides the owner word.
        (
            "  if (old !== w) {\n    m.st(f + S_WAKER, m.wakerClone(w));\n    if (old !== 0) {\n      m.wakerDrop(old);\n    }\n  }\n"
            "  if (s === NEW) {\n"
            "    // Pending in this worker's timer block: its task stays on this worker, unless an offer\n"
            "    // carries it with its sleeps (sched_sleep_carry.velt).\n"
            "    sp.anchor();\n"
            "    const e = t.exec();\n"
            "    m.storeRelaxed(f + S_OWNER, m.ld(e + t.E_CURRENT) + t.T_WORD);\n",
            "  if (old !== w) {\n    m.st(f + S_WAKER, m.wakerClone(w));\n"
            "    // The owner word goes with the waker (`mv.owner`): before the state, for a new sleep.\n"
            "    m.storeRelaxed(f + S_OWNER, mv.owner(cx, w));\n"
            "    if (old !== 0) {\n      m.wakerDrop(old);\n    }\n  }\n"
            "  if (s === NEW) {\n"
            "    // Pending in this worker's timer block: its task stays on this worker, unless an offer\n"
            "    // carries it with its sleeps (sched_sleep_carry.velt).\n"
            "    sp.anchor();\n"
            "    const e = t.exec();\n",
        ),
        # here(): anchors; the owner word is the waker store's.
        (
            "  if (k + t.T_WORD !== m.loadRelaxed(f + S_OWNER)) {\n    sp.anchor();\n    m.storeRelaxed(f + S_OWNER, k + t.T_WORD);\n  }\n  return true;\n",
            "  if (k + t.T_WORD !== m.loadRelaxed(f + S_OWNER)) {\n    sp.anchor();\n  }\n  return true;\n",
        ),
        (
            " * another task polls it: that task stays with it from now on, as the first did (anchored; an\n"
            " * offer carries the sleep along with it, `S_OWNER`). No when another worker's block holds it or\n"
            " * an offered task carries it.\n",
            " * another task polls it: that task stays with it from now on, as the first did (anchored; it\n"
            " * becomes the owner when the poll registers its own waker, `mv.owner`). No when another\n"
            " * worker's block holds it or an offered task carries it.\n",
        ),
        (
            " * The await word (`T_WORD`) of the task that polled it into its worker's timer block, what that\n"
            " * task's contexts point at (`sleepPoll`; sched_sleep_carry.velt `carry`), or 0.\n",
            " * The await word (`T_WORD`) of the task that polled it into its worker's timer block, what that\n"
            " * task's contexts point at (`sleepPoll`), while the waker it is registered with keeps that task\n"
            " * alive (sched_sleep_move.velt `owner`); 0 otherwise. So a word that is not 0 names a live task,\n"
            " * never an address a later task was given.\n",
        ),
    ],
)

edit(
    "sched_sleep_move.velt",
    [
        ('import * as m from "./sched_mem";\n', 'import * as m from "./sched_mem";\nimport * as nd from "./sched_node";\n'),
        (
            "/**\n * A poll of sleep `f`, held by another worker's block, with waker `waker` (borrowed)",
            '''/**
 * The owner word (`S_OWNER`) of a sleep registered with waker `w` through context `cx`: the
 * polling task's await word, which `cx` points at, when `w` is that context's own waker, a
 * task's or a promise's (a context sits inside the object its waker is: `T_CX`, `N_CX`); else 0.
 *
 * The poll's shortcut trusts the word: a poll whose context points at it is the owner's, which
 * is anchored to the worker that holds the sleep (sched_sleep.velt `sleepPoll`). The word is an
 * address, so it may only be kept while the task it names cannot be freed, or a task made later
 * at the same address would pass for the owner on whatever worker it runs (#3207: its expiry
 * was then filed under a task of another worker). The sleep's own waker is what keeps it: a
 * clone of a task's waker is a reference to the task, and one of a promise's keeps the promise,
 * which keeps its task. A waker of anything else (a combinator's, which only passes its
 * poller's context on) keeps no task: no owner word then, and every later poll asks the worker
 * (`here`).
 */
@inline("always")
export function owner(cx: usize, w: usize): usize {
  return w === cx - t.T_CX || w === cx - nd.N_CX ? m.ld(cx) : 0;
}

/**
 * A poll of sleep `f`, held by another worker's block, with waker `waker` (borrowed)''',
        ),
    ],
)

edit(
    "sched_sleep_carry.velt",
    [
        (
            "    m.storeRelaxed(f + sl.S_OWNER, k + t.T_WORD);\n    // Unordered: `insert` stores the state after it, released.\n",
            "    // The owner word only where the sleep's waker keeps `k` alive (sched_sleep_move.velt `owner`).\n"
            "    m.storeRelaxed(f + sl.S_OWNER, wk.taskOf(sl.waiter(f)) === k ? k + t.T_WORD : 0);\n"
            "    // Unordered: `insert` stores the state after it, released.\n",
        ),
    ],
)
