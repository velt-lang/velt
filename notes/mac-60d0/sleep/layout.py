"""#3273: the sleep's words laid out so that a new sleep's stores come in adjacent pairs (the waker
and the owner, the state and the request count), which arm64 writes with one instruction each."""
P = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/sched_sleep.velt"
s = open(P).read()
old_a = s.index("// A sleep: its deadline (nanoseconds), number, waker, links")
old_b = s.index("export const S_SIZE: usize = 96;")
new = """// A sleep: its deadline (nanoseconds), number, waker, owner, links (the pending list, or its task's
// due list), state, requests under way, home worker, and the task whose due list holds it. The
// words a new sleep sets (`sleep`) come in adjacent pairs: deadline and number, waker and owner,
// state and requests.
const S_DEADLINE: usize = 16;
export const S_SEQ: usize = 24;
export const S_WAKER: usize = 32;
/** The task that polled it into its worker's timer block (sched_sleep_carry.velt `carry`). */
export const S_OWNER: usize = 40;
export const S_PREV: usize = 48;
export const S_NEXT: usize = 56;
export const S_STATE: usize = 64;
/** The hand-overs and the drop under way for it, on other workers' lists (sched_sleep_move.velt). */
export const S_ASKS: usize = 72;
/**
 * The worker whose timer block holds it, once it was polled there (read from other threads:
 * sched_sleep_move.velt).
 */
export const S_HOME: usize = 80;
export const S_TASK: usize = 88;
"""
s = s[:old_a] + new + s[old_b:]
old = """  m.st(f + S_WAKER, 0);
  m.st(f + S_STATE, NEW);
  m.st(f + S_OWNER, 0);
  // The links, `S_HOME` and `S_TASK` are written when a state that reads them begins: the links
  // and `S_HOME` with `PENDING` (`insert`), `S_TASK` with `DUE` (`pushDue`).
  m.st(f + S_ASKS, 0);"""
new = """  m.st(f + S_WAKER, 0);
  m.st(f + S_OWNER, 0);
  m.st(f + S_STATE, NEW);
  m.st(f + S_ASKS, 0);
  // The links, `S_HOME` and `S_TASK` are written when a state that reads them begins: the links
  // and `S_HOME` with `PENDING` (`insert`), `S_TASK` with `DUE` (`pushDue`)."""
assert s.count(old) == 1
s = s.replace(old, new)
open(P, "w").write(s)
print("layout")
