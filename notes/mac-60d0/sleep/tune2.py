"""#3207: sleepPoll's common path without a read of the worker: the task that polled the sleep
into its block (S_OWNER) is anchored there with it, so its polls are at home."""
P = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/sched_sleep.velt"
s = open(P).read()
a = s.index("function sleepPoll(f: usize, cx: usize): u32 {")
b = s.index("function sleepDrop(f: usize): void {")
new_poll = """function sleepPoll(f: usize, cx: usize): u32 {
  // Loaded as shared: a worker that gave this sleep up stored its state last
  // (sched_sleep_move.velt `release`).
  const s = m.load(f + S_STATE);
  if (s === SEEN) {
    return 1;
  }
  const w = m.ld(cx + 8);
  if (s !== NEW) {
    // Polled before. The task that polled it into its block (`S_OWNER`) is anchored there with
    // it, so its polls are on that worker; another task's (the promise was handed on) may not be.
    const now = wk.taskOf(w);
    if ((now === 0 || now !== m.ld(f + S_OWNER) || s === CARRIED) && !here(f, s)) {
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
      m.st(f + S_STATE, SEEN);
      return 1;
    }
  }
  const old = m.ld(f + S_WAKER);
  if (old !== w) {
    m.st(f + S_WAKER, m.wakerClone(w));
    if (old !== 0) {
      m.wakerDrop(old);
    }
  }
  if (s === NEW) {
    // Pending in this worker's timer block: its task stays on this worker, unless an offer
    // carries it with its sleeps (sched_sleep_carry.velt).
    sp.anchor();
    const e = t.exec();
    m.st(f + S_OWNER, m.ld(e + t.E_CURRENT));
    m.storeRelease(f + S_HOME, e);
    insert(block(e), f);
  }
  return 0;
}

"""
s = s[:a] + new_poll + s[b:]
old = s[s.index("/**\n * Pending sleep `f` is polled, on its worker, by another task") : s.index("/**\n * The rule of this file, checked in a counted run")]
new_here = """/**
 * Is sleep `f` (state `s`, polled before by another task than the one polling it now) in this
 * worker's block? Then the task polling it stays with it from now on, as the first did (anchored;
 * an offer carries the sleep along with it, `S_OWNER`). Not when another worker's block holds it
 * or an offered task carries it.
 */
@noInline
function here(f: usize, s: usize): bool {
  const e = t.exec();
  if (s === CARRIED || m.load(f + S_HOME) !== e) {
    return false;
  }
  sp.anchor();
  m.st(f + S_OWNER, m.ld(e + t.E_CURRENT));
  return true;
}

"""
assert s.count(old) == 1
s = s.replace(old, new_here)
open(P, "w").write(s)
print("tuned2")
