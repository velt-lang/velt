"""#3273: SEEN numbered next to NEW, so a drop tells the states that hold a list's place (above
SEEN) with one compare."""
P = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/sched_sleep.velt"
s = open(P).read()
old = """// A sleep's states: not polled yet; pending; due (on its task's due list); fired (counted in
// `TM_UNSEEN` until its leaf sees it); seen; carried by an offered task (in its `T_TIMERS`,
// sched_sleep_carry.velt); dropped while a hand-over was under way (sched_sleep_move.velt).
export const NEW: usize = 0;
export const PENDING: usize = 1;
export const DUE: usize = 2;
export const FIRED: usize = 3;
export const SEEN: usize = 4;
export const CARRIED: usize = 5;
export const GONE: usize = 6;"""
new = """// A sleep's states: not polled yet; seen; pending; due (on its task's due list); fired (counted in
// `TM_UNSEEN` until its leaf sees it); carried by an offered task (in its `T_TIMERS`,
// sched_sleep_carry.velt); dropped while a hand-over was under way (sched_sleep_move.velt). The
// two that are on no list come first: those above `SEEN` hold a place somewhere (`sleepDrop`).
export const NEW: usize = 0;
export const SEEN: usize = 1;
export const PENDING: usize = 2;
export const DUE: usize = 3;
export const FIRED: usize = 4;
export const CARRIED: usize = 5;
export const GONE: usize = 6;"""
assert s.count(old) == 1
s = s.replace(old, new)
old = "  if (s !== NEW && s !== SEEN) {"
assert s.count(old) == 1
s = s.replace(old, "  if (s > SEEN) {")
open(P, "w").write(s)
print("states")
