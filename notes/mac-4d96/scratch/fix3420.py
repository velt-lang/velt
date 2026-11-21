"""#3420 review items 1 and 2.  python3 fix3420.py <std>"""
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
    "sched_node.velt",
    [
        (
            "/** Gives back one reference to node `n`: the last frees it. */\n"
            "export function release(n: usize): void {\n"
            "  if (m.dec(n + N_REFS) === 1) {\n"
            "    const task = m.ld(n + N_TASK);\n"
            "    x.free(n);\n"
            "    m.release(n, m.ld32(n + N_BYTES) as usize);\n"
            "    if (task !== 0) {\n"
            "      wk.release(task);\n"
            "    }\n"
            "  }\n"
            "}\n",
            "/**\n"
            " * Gives back one reference to node `n`: the last frees it, and gives back its reference to its\n"
            " * task (sched_node_lost.velt `taskRef`: with a wake when that is not on the task's thread).\n"
            " */\n"
            "export function release(n: usize): void {\n"
            "  if (m.dec(n + N_REFS) === 1) {\n"
            "    const task = m.ld(n + N_TASK);\n"
            "    x.free(n);\n"
            "    m.release(n, m.ld32(n + N_BYTES) as usize);\n"
            "    if (task !== 0) {\n"
            "      lost.taskRef(task);\n"
            "    }\n"
            "  }\n"
            "}\n",
        )
    ],
)

edit(
    "sched_node_lost.velt",
    [
        (
            "  const lone = (fl & nd.FINISHED) !== 0 ? refs === 1 : refs === 2;\n",
            "  // The flags first, then the count. A count of 1 is ours alone, whatever the flags said: the\n"
            "  // node may have finished on its task between the two loads, and the task given its\n"
            "  // reference back. A count of 2 with the node unfinished is ours and the task's.\n"
            "  const lone = refs === 1 || (refs === 2 && (fl & nd.FINISHED) === 0);\n",
        ),
        (
            "/**\n * An adoption poll left started node `n` suspended",
            '''/**
 * A freed node's reference to its task `task` goes (sched_node.velt `release`). On the task's own
 * thread as it always did. Anywhere else (a waker of the node dropped on another worker, the
 * owner's drop there, a holder's) the task is woken with it (`releaseAway`): the reference may
 * be the one that kept the task from being found parked for ever (sched_task_lost.velt
 * `parks`), and no poll of the task would follow to look again.
 */
@inline("always")
export function taskRef(task: usize): void {
  const e = t.here();
  if (e !== 0 && m.ld(e + t.E_CURRENT) === task) {
    wk.release(task);
    return;
  }
  taskRefAway(task);
}

/** `taskRef` off the task's thread: with one thread at the executor, as on it. */
@noInline
function taskRefAway(task: usize): void {
  if (lk.mode() !== lk.LK_ON) {
    wk.release(task);
    return;
  }
  lt.releaseAway(task);
}

/**
 * An adoption poll left started node `n` suspended''',
        ),
    ],
)
