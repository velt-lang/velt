"""#3207: `forget` (an ending program's sleeps given back) leaves sched_sleep.velt for a file of
its own (the 400-line rule); sched_exit.velt, its one caller, imports it."""
import re

H = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/"
s = open(H + "sched_sleep.velt").read()
a = s.index("/**\n * Task `k`, which never runs again")
b = s.index("/** Sleep `f` leaves its worker's block for an offered")
moved = s[a:b].rstrip("\n") + "\n"
open(H + "sched_sleep.velt", "w").write(s[:a] + s[b:])
for name in ("TM_FIRST", "S_NEXT", "S_WAKER", "S_SIZE"):
    moved = re.sub(r"(?<![`A-Za-z_.])%s\b" % name, "sl." + name, moved)
for fn in ("unlinkPending(", "unlinkDue("):
    moved = re.sub(r"(?<![A-Za-z_.`])" + re.escape(fn), "sl." + fn, moved)
moved = moved.replace("export function gone(", "function gone(")
head = '''// velt/lang/hooks (internal): the sleeps of a task that never runs again are given back when the
// program ends on an uncaught error with its counts on (sched_exit.velt), so the count of blocks
// left is the program's own. The sleeps themselves are sched_sleep.velt's.

import * as m from "./sched_mem";
import * as sl from "./sched_sleep";
import * as t from "./sched_task";
import * as wk from "./sched_wake";

'''
open(H + "sched_sleep_forget.velt", "w").write(head + moved)
x = open(H + "sched_exit.velt").read()
assert x.count("    sl.forget(tb, k);") == 1
x = x.replace("    sl.forget(tb, k);", "    sf.forget(tb, k);")
assert x.count('import * as sl from "./sched_sleep";\n') == 1
x = x.replace('import * as sl from "./sched_sleep";\n', 'import * as sl from "./sched_sleep";\nimport * as sf from "./sched_sleep_forget";\n')
open(H + "sched_exit.velt", "w").write(x)
print("split2")
