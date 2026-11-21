"""#3323 prototype, step 1 (no counted-list unlink yet): python3 proto3323.py <std>"""
import sys

root = sys.argv[1] + "/lang/hooks/"


def edit(name, pairs):
    p = root + name
    s = open(p).read()
    for old, new in pairs:
        assert s.count(old) == 1, (name, old[:60], s.count(old))
        s = s.replace(old, new)
    open(p, "w").write(s)


# The other-side drop hands its reference to the task.
edit(
    "sched_node.velt",
    [
        (
            "      m.wakerDrop(w);\n    }\n    release(n);\n    return;\n  } else if ((fl & FINISHED) === 0) {",
            "      m.wakerDrop(w);\n    }\n    lost.droppedAway(n);\n    return;\n  } else if ((fl & FINISHED) === 0) {",
        )
    ],
)

edit(
    "sched_node_lost.velt",
    [
        ('import * as nc from "./sched_node_catch";\n', 'import * as lk from "./sched_lock";\nimport * as nc from "./sched_node_catch";\nimport * as nw from "./sched_node_wake";\n'),
        (
            "/**\n * An adoption poll left started node `n` suspended",
            '''/**
 * The end of sched_node.velt `nodeDrop` for started node `n` whose owner is not on its task's
 * thread (the promise was handed to another task, which drops it): the owner's reference goes.
 * Two cases leave the node's task with nothing to poll it again (#3323), and in both the
 * reference is given to the task instead, on its woken stack, with the task woken:
 * - `n` is suspended and its task's is the only other reference: nothing can wake it any more,
 *   but only its task's thread may give it up. The task's poll runs it as a woken node, and that
 *   run finds it parked with its owner gone (sched_node.velt `run`, `parked`).
 * - `n` finished and this is its last reference: freeing it here would give its reference to
 *   its task back on this thread, with no poll after it, and a task that waits for that count
 *   to park for ever (sched_task_lost.velt `parks`) would never look again. The task's poll
 *   frees it instead.
 * The task is kept meanwhile by a reference of its own, given back with a wake
 * (sched_task_lost.velt `releaseAway`). With one thread at the executor (the owner is a thread
 * that is no worker) the reference just goes, as before.
 */
@noInline
export function droppedAway(n: usize): void {
  const task = m.ld(n + nd.N_TASK);
  const fl = m.load32(n + nd.N_FLAGS);
  const refs = m.load(n + nd.N_REFS);
  const lone = (fl & nd.FINISHED) !== 0 ? refs === 1 : refs === 2;
  if (!lone || task === 0 || lk.mode() !== lk.LK_ON) {
    nd.release(n);
    return;
  }
  m.inc(task + t.T_REFS);
  nw.give(n);
  lt.releaseAway(task);
}

/**
 * An adoption poll left started node `n` suspended''',
        ),
    ],
)

edit(
    "sched_node_wake.velt",
    [
        (
            "/** Wakes node `n`: once among its task's woken nodes",
            '''/**
 * Gives started node `n` to its task with the caller's reference: among its task's woken nodes,
 * the reference the stack's from then on (a node that is there already just loses it), finished
 * or not. The caller keeps the task alive meanwhile and wakes it (sched_node_lost.velt
 * `droppedAway`).
 */
export function give(n: usize): void {
  let fl = m.load32(n + nd.N_FLAGS);
  while (true) {
    if ((fl & nd.QUEUED) !== 0) {
      nd.release(n);
      return;
    }
    const was = m.cas32(n + nd.N_FLAGS, fl, fl | nd.QUEUED);
    if (was === fl) {
      break;
    }
    fl = was;
  }
  const task = m.ld(n + nd.N_TASK);
  m.pushStack(task + t.T_WOKEN, n, nd.N_NEXT);
  if (m.load32(task + t.T_STATE) === t.DONE) {
    // Pushed too late, as in `wake`.
    wn.giveBackWoken(task);
  }
}

/** Wakes node `n`: once among its task's woken nodes''',
        )
    ],
)

edit(
    "sched_task_lost.velt",
    [
        (
            "// The rule as assertions, with the block counters on",
            '''/**
 * Gives back a reference to task `k` from a thread that is not polling it, with a wake: the poll
 * that follows ends with the count as it is now (`parks`), and takes what the caller put on the
 * task's woken stack. With the join slot's lock held from before the count goes down until after
 * the wake: a task is not freed while that lock is held (sched_wake.velt `release`), so the wake
 * touches a live task whatever the task does meanwhile. The last reference frees the task, with
 * the lock given back first (nobody else can reach the task then).
 */
@noInline
export function releaseAway(k: usize): void {
  const kind = lk.kind();
  const s = mk.lockJoin(k, kind);
  const last = m.dec(k + t.T_REFS) === 1;
  if (!last) {
    wk.wake(k);
  }
  mk.unlockJoin(k, s, kind);
  if (last) {
    wk.freed(k);
  }
}

// The rule as assertions, with the block counters on''',
        )
    ],
)

edit(
    "sched_wake.velt",
    [
        (
            "export function release(t: usize): void {\n  if (m.dec(t + tk.T_REFS) === 1) {\n",
            "export function release(t: usize): void {\n  if (m.dec(t + tk.T_REFS) === 1) {\n"
            "    if ((m.load32(t + tk.T_JOIN_STATE) & tk.J_LOCK) !== 0) {\n"
            "      // Another thread gives a reference back with a wake (sched_task_lost.velt\n"
            "      // `releaseAway`): it touches the task until it gives the lock back.\n"
            "      awaitUnlocked(t);\n"
            "    }\n",
        ),
        (
            "/** The exception of task `t`, whose root panicked: out of line",
            "/** Frees task `t`, whose last reference the caller gave back by its own count (`release`'s end). */\n"
            "export function freed(t: usize): void {\n"
            "  if ((m.ld32(t + tk.T_FLAGS) & nc.T_PANICKED) !== 0) {\n"
            "    freePanicked(t);\n"
            "  }\n"
            "  m.release(t, m.ld(t + tk.T_BYTES));\n"
            "}\n\n"
            "/** Waits until the join slot's lock of task `t`, about to be freed, is given back. */\n"
            "@noInline\n"
            "function awaitUnlocked(t: usize): void {\n"
            "  while ((m.load32(t + tk.T_JOIN_STATE) & tk.J_LOCK) !== 0) {\n"
            "    m.yieldCore();\n"
            "  }\n"
            "}\n\n"
            "/** The exception of task `t`, whose root panicked: out of line",
        ),
    ],
)
