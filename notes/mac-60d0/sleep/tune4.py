"""#3273: the poll of a sleep polled before finds its poller in the poll's context (no read of
the worker on the owner's path); a carried sleep has no owner until it is adopted, so the same
compare sends its polls the slow way; S_HOME needs no store at creation."""
H = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/"
s = open(H + "sched_sleep.velt").read()
a = s.index("    // Polled before. The task that polled it into its block (`S_OWNER`) is anchored there with")
b = s.index("  const old = m.ld(f + S_WAKER);")
s = (
    s[:a]
    + """    // Polled before. The task that polled it into its block (`S_OWNER`) is anchored there with
    // it, so its polls are on that worker; another task's (the promise was handed on) may not
    // be. The context names the task polling: its first word points at that task's `T_WORD`,
    // in a task's own context, in a promise's (always polled by the task that drives it, which
    // a transfer changes together with this word: sched_node.velt) and in a combinator's, which
    // passes its poller's on (sched_all.velt). Any other context, and a carried sleep (it has
    // no owner until its task adopts it), take the slow way, which asks the worker.
    if (m.ld(cx) - t.T_WORD !== m.ld(f + S_OWNER) && !here(f, s)) {
      // Held by another worker's block, or carried by an offered task: its worker gives it up,
      // and this task polls it again as a new one.
      return mv.ask(f, w);
    }
    if (s === FIRED) {
      seenOn(t.exec(), f);
      return 1;
    }
    if (s === DUE && firesItself(t.exec(), f)) {
      unlinkDue(m.ld(f + S_TASK), f);
      m.storeRelease(f + S_STATE, SEEN);
      return 1;
    }
  }
"""
    + s[b:]
)
a = s.index("/**\n * Sleep `f` (state `s`), polled before, is polled on worker `e` by task `k`, not the one that")
b = s.index("/**\n * The rule of this file, checked in a counted run")
s = (
    s[:a]
    + """/**
 * Sleep `f` (state `s`), polled before, is polled through a context that does not name its owner:
 * is it in this worker's block? Yes when the task this worker is polling is the owner after all
 * (a context that is not the executor's). Yes too when the sleep is in this worker's block and
 * another task polls it: that task stays with it from now on, as the first did (anchored; an
 * offer carries the sleep along with it, `S_OWNER`). No when another worker's block holds it or
 * an offered task carries it.
 */
@noInline
function here(f: usize, s: usize): bool {
  const e = t.exec();
  const k = m.ld(e + t.E_CURRENT);
  if (s === CARRIED || m.load(f + S_HOME) !== e) {
    return false;
  }
  if (k !== m.ld(f + S_OWNER)) {
    sp.anchor();
    m.st(f + S_OWNER, k);
  }
  return true;
}

"""
    + s[b:]
)
old = "  m.st(f + S_OWNER, 0);\n  m.st(f + S_HOME, 0);\n  m.st(f + S_ASKS, 0);\n"
assert s.count(old) == 1
s = s.replace(old, "  m.st(f + S_OWNER, 0);\n  // `S_HOME` is written when a poll registers it, and read only after that.\n  m.st(f + S_ASKS, 0);\n")
old = "export function carried(b: usize, f: usize): void {\n  unlinkPending(b, f);\n"
assert s.count(old) == 1
s = s.replace(old, "export function carried(b: usize, f: usize): void {\n  unlinkPending(b, f);\n  // No owner while it is carried: a poll of it goes the slow way (`sleepPoll`).\n  m.st(f + S_OWNER, 0);\n")
open(H + "sched_sleep.velt", "w").write(s)
c = open(H + "sched_sleep_carry.velt").read()
old = "    m.storeRelease(f + sl.S_HOME, e);\n    sl.insert(b, f);\n"
assert c.count(old) == 1
c = c.replace(old, "    m.st(f + sl.S_OWNER, k);\n    m.storeRelease(f + sl.S_HOME, e);\n    sl.insert(b, f);\n")
open(H + "sched_sleep_carry.velt", "w").write(c)
print("tuned4")
