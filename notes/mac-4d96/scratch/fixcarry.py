"""#3273: carry's owner() no longer reads the owner word. python3 fixcarry.py <std>"""
import sys

p = sys.argv[1] + "/lang/hooks/sched_sleep_carry.velt"
s = open(p).read()
old = s[s.index("/**\n * The offered task pending sleep `f` goes with, or 0.") : s.index("/** Worker `e`, about to poll task `k`, takes the sleeps it carries")]
new = '''/**
 * The offered task pending sleep `f` goes with, or 0. Its expiry touches the task its waker wakes
 * (sched_sleep.velt `expire`: that task's due list and flags, from the expiring worker's thread),
 * so a sleep whose waker is a task's goes with that task alone: one handed on to another task
 * (polled again there) stays with the task that polls it now. A sleep whose waker is not a
 * task's (a combinator's) goes with no task: nothing keeps the task that polled it into the
 * block alive, so the sleep names none (sched_sleep_move.velt `owner`). It stays in this block,
 * its waker is woken from any thread when it expires, and a poll of it on another worker asks
 * this one for it, as for any handed-on sleep.
 */
function owner(f: usize): usize {
  const w = wk.taskOf(sl.waiter(f));
  return w !== 0 && t.has(w, t.CARRIED) ? w : 0;
}

'''
s = s.replace(old, new)
open(p, "w").write(s)
