"""#3323 prototype, step 2: the counted run's list under the join lock; the dropper unlinks.
python3 proto3323b.py <std>"""
import sys

root = sys.argv[1] + "/lang/hooks/"


def edit(name, pairs):
    p = root + name
    s = open(p).read()
    for old, new in pairs:
        assert s.count(old) == 1, (name, old[:60], s.count(old))
        s = s.replace(old, new)
    open(p, "w").write(s)


edit(
    "sched_task_lost.velt",
    [
        # link: under the lock
        (
            "function linkCounted(task: usize, n: usize): void {\n  checkLink(task, n);\n  nd.setFlags(n, NLINKED);\n",
            "function linkCounted(task: usize, n: usize): void {\n  checkLink(task, n);\n"
            "  const kind = lk.kind();\n  const s = mk.lockJoin(task, kind);\n  nd.setFlags(n, NLINKED);\n",
        ),
        (
            "    m.st(head + nd.N_TAKEN_NEXT, n);\n  }\n  m.st(task + t.T_LOST, n);\n}",
            "    m.st(head + nd.N_TAKEN_NEXT, n);\n  }\n  m.st(task + t.T_LOST, n);\n  mk.unlockJoin(task, s, kind);\n}",
        ),
        # unlink: under the lock, by whoever takes NLINKED off
        (
            "@noInline\nfunction unlinkCounted(task: usize, n: usize): void {\n  const next = m.ld(n + nd.N_NEXT);",
            "@noInline\nfunction unlinkCounted(task: usize, n: usize): void {\n"
            "  const kind = lk.kind();\n  const s = mk.lockJoin(task, kind);\n"
            "  if ((m.and32(n + nd.N_FLAGS, ~NLINKED) & NLINKED) !== 0) {\n    unlinkLocked(task, n);\n  }\n"
            "  mk.unlockJoin(task, s, kind);\n}\n\n"
            "/**\n * Node `n`, dropped on another thread than its task `task`'s while it is on the task's list\n"
            " * (sched_node_lost.velt `droppedAway`), leaves the links, which the task's woken stack is\n"
            " * about to use: by the dropper, under the lock that guards the list. It stays counted\n"
            " * (`LISTED`): the count is the task's thread's, which takes it down when it gives the node up.\n */\n"
            "export function unlinkAway(task: usize, n: usize): void {\n  unlinkCounted(task, n);\n}\n\n"
            "/** `unlinkCounted` with the list's lock held, `n`'s `NLINKED` taken off by the caller. */\n"
            "function unlinkLocked(task: usize, n: usize): void {\n  const next = m.ld(n + nd.N_NEXT);",
        ),
        # dead(): the decision and the walk under the lock
        (
            "  mk.unlockJoin(k, s, kind);\n  if (refs !== l + 1 || m.load32(k + t.T_STATE) !== t.RUNNING) {\n    return false;\n  }\n"
            "  if (t.has(k, t.LINKED)) {\n    checkListDead(k, l);\n  }\n  t.flags(k, t.CANCEL, 0);\n  return true;\n}",
            "  // The rest with the lock held: a promise dropped on another thread leaves the list under\n"
            "  // it (`unlinkAway`), with a reference to the task taken before, so either the count above\n"
            "  // shows that reference or the walk below still finds the promise.\n"
            "  if (refs !== l + 1 || m.load32(k + t.T_STATE) !== t.RUNNING || m.load(k + t.T_WOKEN) !== 0) {\n"
            "    mk.unlockJoin(k, s, kind);\n    return false;\n  }\n"
            "  if (t.has(k, t.LINKED)) {\n    checkListDead(k, l);\n  }\n  t.flags(k, t.CANCEL, 0);\n  mk.unlockJoin(k, s, kind);\n  return true;\n}",
        ),
    ],
)

edit(
    "sched_node_lost.velt",
    [
        (
            "  m.inc(task + t.T_REFS);\n  nw.give(n);\n",
            "  m.inc(task + t.T_REFS);\n"
            "  if ((fl & lt.NLINKED) !== 0) {\n"
            "    // On its task's list of owner-only promises, linked through the woken stack's links (a\n"
            "    // counted run): off the list first.\n"
            "    lt.unlinkAway(task, n);\n"
            "  }\n"
            "  nw.give(n);\n",
        )
    ],
)
