"""#3273 review item 1: say in the unlinks docs that the state is the caller's to store."""
P = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/sched_sleep.velt"
s = open(P).read()
a = "export function unlinkPending(b: usize, f: usize): void {"
b = "export function unlinkDue(k: usize, f: usize): void {"
assert s.count(a) == 1 and s.count(b) == 1
s = s.replace(a, "/**\n * Takes pending sleep `f` out of block `b`. Its state is left as it was: the caller stores the\n * state it means (or frees the sleep). No state in between, since a task on another worker may\n * read it (`sleepPoll`), and a new sleep is one it may register (#3207).\n */\n" + a)
s = s.replace(b, "/** Takes due sleep `f` off task `k`'s due list; its state is the caller's to store, as `unlinkPending`'s. */\n" + b)
open(P, "w").write(s)
