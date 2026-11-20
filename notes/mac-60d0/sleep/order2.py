"""#3273: the ordering argument moves into `insert`'s doc comment (std-lint counts `//` lines)."""
P = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/sched_sleep.velt"
s = open(P).read()
a = s.index("  // Released: the registration this ends")
b = s.index("  m.storeRelease(f + S_STATE, PENDING);")
s = s[:a] + "  // Released, last (the doc comment above).\n" + s[b:]
old = "/** Puts sleep `f` among the pending ones of block `b`, in order (its place found from the end). */\n"
new = """/**
 * Puts sleep `f` among the pending ones of block `b`, in order (its place found from the end).
 *
 * The state is stored last, released: the registration this ends (`S_OWNER`, `S_HOME`, the
 * links) comes with it to a task on another worker that reads the state (`sleepPoll`,
 * `sleepDrop`, sched_sleep_move.velt `release`). The holder's later states, `DUE` and `FIRED`,
 * are plain stores. A worker that reads one only asks the holder for the sleep (it is not
 * `SEEN`), and the holder reads it on its own thread. Its `S_HOME` is this registration's, seen
 * with `PENDING`, or stale: an older value names another worker (which asks, and the holder
 * answers), or its own worker, which stored that itself, before the hand-over whose state it
 * stored itself too. `SEEN` stays released: a worker that reads it may free the sleep.
 */
"""
assert s.count(old) == 1
s = s.replace(old, new)
old = "    // Plain: `insert` stores the state last, released, and a worker that reads the state\n    // reads this after it.\n"
assert s.count(old) == 1
s = s.replace(old, "    // Plain: `insert` stores the state after it, released.\n")
open(P, "w").write(s)
print("ok")
