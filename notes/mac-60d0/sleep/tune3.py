"""#3207: who polls is the task the worker is polling (E_CURRENT), nothing a context or a waker
says: a task awaiting a promise another task owns polls with that promise's context."""
P = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/sched_sleep.velt"
s = open(P).read()
a = s.index("    // Polled before. The task that polled it into its block (`S_OWNER`) is anchored there with")
b = s.index("  const old = m.ld(f + S_WAKER);")
s = (
    s[:a]
    + """    // Polled before. The task that polled it into its block (`S_OWNER`) is anchored there with
    // it, so its polls are on that worker; another task's (the promise was handed on) may not be.
    // Which task polls is what the worker is polling: a promise's context and waker name the
    // task that owns the promise, also when another task awaiting it polls it.
    const e = t.exec();
    const k = m.ld(e + t.E_CURRENT);
    if ((k !== m.ld(f + S_OWNER) || s === CARRIED) && !handedOn(e, f, s, k)) {
      // Held by another worker's block, or carried by an offered task: its worker gives it up,
      // and this task polls it again as a new one.
      return mv.ask(f, w);
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
"""
    + s[b:]
)
a = s.index("/**\n * Is sleep `f` (state `s`, polled before by another task than the one polling it now) in this")
b = s.index("/**\n * The rule of this file, checked in a counted run")
s = (
    s[:a]
    + """/**
 * Sleep `f` (state `s`), polled before, is polled on worker `e` by task `k`, not the one that
 * polled it into its block: is it in this worker's block? Then `k` stays with it from now on, as
 * the first did (anchored; an offer carries the sleep along with it, `S_OWNER`). Not when another
 * worker's block holds it or an offered task carries it.
 */
@noInline
function handedOn(e: usize, f: usize, s: usize, k: usize): bool {
  if (s === CARRIED || m.load(f + S_HOME) !== e) {
    return false;
  }
  sp.anchor();
  m.st(f + S_OWNER, k);
  return true;
}

"""
    + s[b:]
)
open(P, "w").write(s)
print("tuned3")
