"""Moves the out-of-line part of sleepDrop to sched_sleep_move.velt (the file-size rule)."""
import sys

root = sys.argv[1] + "/lang/hooks/"
p = root + "sched_sleep.velt"
s = open(p).read()
a = s.index("/** The drop of a sleep that holds a place")
b = s.index("/**\n * Sleep `f` (state `s`), polled before")
s = s[:a] + s[b:]
old = "    dropHeld(f, s, asks);\n"
assert s.count(old) == 1
s = s.replace(old, "    mv.dropHeld(f, s, asks);\n")
open(p, "w").write(s)

p = root + "sched_sleep_move.velt"
s = open(p).read()
old = 'import * as sl from "./sched_sleep";\n'
assert s.count(old) == 1
s = s.replace(old, 'import * as sl from "./sched_sleep";\nimport * as rl from "./sched_sleep_rule";\n')
anchor = "function post(f: usize, waker: usize): void {"
assert s.count(anchor) == 1
new = """/**
 * The drop of sleep `f` when it holds a place (state `s` above `SEEN`) or requests name it
 * (`asks`): what sched_sleep.velt `sleepDrop` keeps out of line.
 */
@noInline
export function dropHeld(f: usize, s: usize, asks: usize): void {
  if (asks !== 0) {
    // A hand-over under way names it: the worker that one reaches frees it.
    dropAway(f);
    return;
  }
  const e = t.exec();
  if (s === sl.CARRIED || m.load(f + sl.S_HOME) !== e) {
    // Its worker takes it off its list and frees it.
    dropAway(f);
    return;
  }
  if (wk.counting() && m.ld(f + sl.S_HOME) !== e) {
    rl.held("is dropped");
  }
  if (s === sl.PENDING) {
    sl.unlinkPending(sl.block(e), f);
  } else if (s === sl.DUE) {
    sl.unlinkDue(m.ld(f + sl.S_TASK), f);
  } else {
    sl.seenOn(e, f);
  }
  const w = m.ld(f + sl.S_WAKER);
  if (w !== 0) {
    m.wakerDrop(w);
  }
  m.release(f, sl.S_SIZE);
}

"""
s = s.replace(anchor, new + anchor)
open(p, "w").write(s)
