"""#3207: sleepPoll/sleepDrop read the worker once; the checks of the rule off the list operations."""
P = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/sched_sleep.velt"
s = open(P).read()


def edit(old, new):
    global s
    assert s.count(old) == 1, (old[:60], s.count(old))
    s = s.replace(old, new)


edit(
    """  const s = m.load(f + S_STATE);
  if (s !== NEW && s !== SEEN && away(f, s)) {
    // Held by another worker's block (handed on by the task that polled it there): that
    // worker gives it up, and this task polls it again as a new one.
    return mv.ask(f, m.ld(cx + 8));
  }
  if (s >= FIRED) {
    if (s === FIRED) {
      seen(f);
    }
    return 1;
  }
  if (s === DUE && firesItself(f)) {
    unlinkDue(m.ld(f + S_TASK), f);
    m.st(f + S_STATE, SEEN);
    return 1;
  }
""",
    """  const s = m.load(f + S_STATE);
  if (s === SEEN) {
    return 1;
  }
  if (s !== NEW) {
    const e = t.exec();
    if (s === CARRIED || m.load(f + S_HOME) !== e) {
      // Held by another worker's block (handed on by the task that polled it there), or carried
      // by an offered task: its worker gives it up, and this task polls it again as a new one.
      return mv.ask(f, m.ld(cx + 8));
    }
    if (s === FIRED) {
      seenOn(e, f);
      return 1;
    }
    if (s === DUE && firesItself(e, f)) {
      unlinkDue(m.ld(f + S_TASK), f);
      m.st(f + S_STATE, SEEN);
      return 1;
    }
  }
""",
)
edit(
    """  const s = m.load(f + S_STATE);
  if (m.load(f + S_ASKS) !== 0 || (s !== NEW && s !== SEEN && away(f, s))) {
    // Its worker (or the one a hand-over under way reaches) takes it off its list and frees it.
    mv.dropAway(f);
    return;
  }
  if (s === PENDING) {
    unlinkPending(block(t.exec()), f);
  } else if (s === DUE) {
    unlinkDue(m.ld(f + S_TASK), f);
  } else if (s === FIRED) {
    seen(f);
  }
""",
    """  const s = m.load(f + S_STATE);
  if (m.load(f + S_ASKS) !== 0) {
    // A hand-over under way names it: the worker that one reaches frees it.
    mv.dropAway(f);
    return;
  }
  if (s !== NEW && s !== SEEN) {
    const e = t.exec();
    if (s === CARRIED || m.load(f + S_HOME) !== e) {
      // Its worker takes it off its list and frees it.
      mv.dropAway(f);
      return;
    }
    if (wk.counting() && m.ld(f + S_HOME) !== e) {
      held("is dropped");
    }
    if (s === PENDING) {
      unlinkPending(block(e), f);
    } else if (s === DUE) {
      unlinkDue(m.ld(f + S_TASK), f);
    } else {
      seenOn(e, f);
    }
  }
""",
)
edit(
    """/**
 * Is sleep `f` (state `s`, polled before) out of this worker's reach: in another worker's block, or
 * carried by an offered task?
 */
@inline
function away(f: usize, s: usize): bool {
  return s === CARRIED || m.load(f + S_HOME) !== t.exec();
}

""",
    "",
)
edit(
    """/** Fired sleep `f` was seen (polled ready, or dropped). */
export function seen(f: usize): void {
  const b = block(t.exec());
  m.st(b + TM_UNSEEN, m.ld(b + TM_UNSEEN) - 1);
  m.st(f + S_STATE, SEEN);
}
""",
    """/** Fired sleep `f` was seen (polled ready, or dropped) on its worker `e`. */
@inline
export function seenOn(e: usize, f: usize): void {
  const b = block(e);
  m.st(b + TM_UNSEEN, m.ld(b + TM_UNSEEN) - 1);
  m.st(f + S_STATE, SEEN);
}
""",
)
edit(
    """function firesItself(f: usize): bool {
  const k = m.ld(f + S_TASK);
  const e = t.exec();
  return (""",
    """function firesItself(e: usize, f: usize): bool {
  const k = m.ld(f + S_TASK);
  return (""",
)
edit(
    """export function unlinkPending(b: usize, f: usize): void {
  if (wk.counting() && m.ld(f + S_HOME) !== t.exec()) {
    held("leaves a block");
  }
""",
    "export function unlinkPending(b: usize, f: usize): void {\n",
)
edit(
    """      if (wk.counting() && m.ld(k + t.T_HOME) !== t.exec()) {
        held("expires for a task");
      }
""",
    """      if (check && m.ld(k + t.T_HOME) !== t.exec()) {
        held("expires for a task");
      }
""",
)
edit(
    """export function expire(b: usize, now: u64): u64 {
  let f = m.ld(b + TM_FIRST);
""",
    """export function expire(b: usize, now: u64): u64 {
  let f = m.ld(b + TM_FIRST);
  const check = wk.counting();
""",
)
open(P, "w").write(s)
M = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/sched_sleep_move.velt"
v = open(M).read()
assert v.count("    sl.seen(f);\n") == 1
open(M, "w").write(v.replace("    sl.seen(f);\n", "    sl.seenOn(e, f);\n"))
print("tuned")
