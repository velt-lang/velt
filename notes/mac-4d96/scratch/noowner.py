"""#3273: no owner word. A due sleep's poll is checked against the task it is due for (S_TASK,
kept alive by the sleep's waker); a pending one asks `here`.  python3 noowner.py <std>"""
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
            "export const S_TASK: usize = 64;\n"
            "/**\n"
            " * The await word (`T_WORD`) of the task that polled it into its worker's timer block, what that\n"
            " * task's contexts point at (`sleepPoll`), while the waker it is registered with keeps that task\n"
            " * alive (sched_sleep_move.velt `owner`); 0 otherwise. So a word that is not 0 names a live task,\n"
            " * never an address a later task was given.\n"
            " */\n"
            "export const S_OWNER: usize = 72;\n",
            "export const S_TASK: usize = 64;\n",
        ),
        (
            "  // The links, `S_OWNER`, `S_HOME` and `S_TASK` are written when a state that reads them\n"
            "  // begins: the links, `S_OWNER` and `S_HOME` with `PENDING` (`sleepPoll`, `insert`; `adopt`),\n"
            "  // `S_TASK` with `DUE` (`pushDue`).\n",
            "  // The links, `S_HOME` and `S_TASK` are written when a state that reads them begins: the links\n"
            "  // and `S_HOME` with `PENDING` (`sleepPoll`, `insert`; `adopt`), `S_TASK` with `DUE` (`pushDue`).\n",
        ),
        (
            "    // Polled before. The task that polled it into its block (`S_OWNER`) is anchored there with\n"
            "    // it, so its polls are on that worker; another task's (the promise was handed on) may not\n"
            "    // be. The context names the task polling: its first word points at that task's `T_WORD`,\n"
            "    // in a task's own context, in a promise's (always polled by the task that drives it, which\n"
            "    // a transfer changes together with this word: sched_node.velt) and in a combinator's, which\n"
            "    // passes its poller's on (sched_all.velt). Any other context, and a carried sleep (it has\n"
            "    // no owner until its task adopts it), take the slow way, which asks the worker.\n",
            "    // Polled before: is it in this worker's block? The task that polled it there is anchored\n"
            "    // there with it; another task's poll (the promise was handed on) may run anywhere.\n",
        ),
        (
            "    if (m.ld(cx) !== m.loadRelaxed(f + S_OWNER) && !here(f, s)) {\n"
            "      // Held by another worker's block, or carried by an offered task: its worker gives it up,\n"
            "      // and this task polls it again as a new one.\n"
            "      return mv.ask(f, m.ld(cx + 8));\n"
            "    }\n"
            "    if (s === DUE && firesItself(t.exec(), f)) {\n"
            "      unlinkDue(m.ld(f + S_TASK), f);\n"
            "      m.storeRelease(f + S_STATE, SEEN);\n"
            "      return 1;\n"
            "    }\n",
            "    if (s === DUE) {\n"
            "      // Due for a task (`S_TASK`, the one its waker wakes: `expire`), which that waker keeps\n"
            "      // alive until the sleep fires. So a poll whose context names that task (its first word\n"
            "      // points at the polling task's `T_WORD`: a task's own context, a promise's, and a\n"
            "      // combinator's, which passes its poller's on) is that task's and no other's: an address\n"
            "      // compared here is never one a task made later was given (#3207). That task registered\n"
            "      // its waker in a poll on the worker that holds the sleep and is anchored there. Any\n"
            "      // other poll asks the worker.\n"
            "      if (m.ld(cx) !== m.ld(f + S_TASK) + t.T_WORD && !here(f, s)) {\n"
            "        return mv.ask(f, m.ld(cx + 8));\n"
            "      }\n"
            "      if (firesItself(t.exec(), f)) {\n"
            "        unlinkDue(m.ld(f + S_TASK), f);\n"
            "        m.storeRelease(f + S_STATE, SEEN);\n"
            "        return 1;\n"
            "      }\n"
            "    } else if (!here(f, s)) {\n"
            "      // Pending in another worker's block, or carried by an offered task: its worker gives it\n"
            "      // up, and this task polls it again as a new one. A pending sleep keeps no word of the\n"
            "      // task that polled it there: nothing would keep that task alive (its waker may be a\n"
            "      // combinator's), and a task made later at its address would pass for it.\n"
            "      return mv.ask(f, m.ld(cx + 8));\n"
            "    }\n",
        ),
        (
            "    // The owner word goes with the waker (`mv.owner`), before the state for a new sleep. Stored\n"
            "    // first, so the context is not needed after the clone's call: the task it names is the one\n"
            "    // polling, alive without the waker for these two steps.\n"
            "    m.storeRelaxed(f + S_OWNER, mv.owner(cx, w));\n",
            "",
        ),
        (
            "/**\n"
            " * Sleep `f` (state `s`), polled before, is polled through a context that does not name its owner:\n"
            " * is it in this worker's block? Yes when the task this worker is polling is the owner after all\n"
            " * (a context that is not the executor's). Yes too when the sleep is in this worker's block and\n"
            " * another task polls it: that task stays with it from now on, as the first did (anchored; it\n"
            " * becomes the owner when the poll registers its own waker, `mv.owner`). No when another\n"
            " * worker's block holds it or an offered task carries it.\n"
            " */\n",
            "/**\n"
            " * Sleep `f` (state `s`), polled before: is it in this worker's block? Then the task polling it\n"
            " * stays with it from now on, as the first that polled it did (anchored). No when another\n"
            " * worker's block holds it or an offered task carries it.\n"
            " */\n",
        ),
        (
            "  const e = t.exec();\n  const k = m.ld(e + t.E_CURRENT);\n  if (s === CARRIED || m.load(f + S_HOME) !== e) {\n    return false;\n  }\n"
            "  if (k + t.T_WORD !== m.loadRelaxed(f + S_OWNER)) {\n    sp.anchor();\n  }\n  return true;\n",
            "  if (s === CARRIED || m.load(f + S_HOME) !== t.exec()) {\n    return false;\n  }\n  sp.anchor();\n  return true;\n",
        ),
        (
            "  // No owner while it is carried: a poll of it goes the slow way (`sleepPoll`).\n  m.storeRelaxed(f + S_OWNER, 0);\n",
            "",
        ),
        (
            "  m.st(f + S_TASK, k);\n  // Unordered: see `insert`.\n  m.storeRelaxed(f + S_STATE, DUE);\n",
            "  m.st(f + S_TASK, k);\n"
            "  // Released: a poll on another worker that reads `DUE` reads this `S_TASK` (`sleepPoll`).\n"
            "  m.storeRelease(f + S_STATE, DUE);\n",
        ),
    ],
)

p = root + "sched_sleep_move.velt"
s = open(p).read()
a = s.index("/**\n * The owner word (`S_OWNER`) of a sleep registered with waker `w`")
b = s.index("/**\n * A poll of sleep `f`, held by another worker's block, with waker `waker` (borrowed)")
s = s[:a] + s[b:]
for old, new in (
    ('import { inline, noInline } from "velt/lang/intrinsics";\n', 'import { noInline } from "velt/lang/intrinsics";\n'),
    ('import * as nd from "./sched_node";\n', ""),
):
    assert s.count(old) == 1, old
    s = s.replace(old, new)
open(p, "w").write(s)

edit(
    "sched_sleep_carry.velt",
    [
        (
            "    // The owner word only where the sleep's waker keeps `k` alive (sched_sleep_move.velt `owner`).\n"
            "    m.storeRelaxed(f + sl.S_OWNER, wk.taskOf(sl.waiter(f)) === k ? k + t.T_WORD : 0);\n",
            "",
        ),
        (
            " * task's (a combinator's) goes with no task: nothing keeps the task that polled it into the\n"
            " * block alive, so the sleep names none (sched_sleep_move.velt `owner`). It stays in this block,\n",
            " * task's (a combinator's) goes with no task: nothing keeps the task that polled it into the\n"
            " * block alive, so the sleep names none (sched_sleep.velt `sleepPoll`). It stays in this block,\n",
        ),
    ],
)
