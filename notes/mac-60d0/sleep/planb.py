"""#3273 plan B: a due sleep that fires itself is on its worker already (the task polling is the
one whose due list holds it, running here), so the owner check moves after that path."""
P = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/sched_sleep.velt"
s = open(P).read()
old_a = s.index("  if (s !== NEW) {\n    // Polled before.")
old_b = s.index("  const old = m.ld(f + S_WAKER);")
new = """  if (s === DUE && firesItself(t.exec(), f)) {
    // On its worker: the task polling it is the one whose due list holds it, running here.
    unlinkDue(m.ld(f + S_TASK), f);
    m.storeRelease(f + S_STATE, SEEN);
    return 1;
  }
  if (s !== NEW) {
    // Polled before. The task that polled it into its block (`S_OWNER`) is anchored there with
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
  }
"""
s = s[:old_a] + new + s[old_b:]
open(P, "w").write(s)
print("plan B")
