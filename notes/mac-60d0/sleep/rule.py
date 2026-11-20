"""#3273: the rule check of sched_sleep.velt moves to sched_sleep_rule.velt (std-lint's size)."""
P = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/sched_sleep.velt"
s = open(P).read()


def cut(start, end):
    global s
    a = s.index(start)
    b = s.index(end, a)
    s = s[:a] + s[b:]


cut("/**\n * The rule of this file, checked in a counted run", "/** Fired sleep `f` was seen")
cut("/**\n * The rule (`held`) for the sleeps of block `b`", "/**\n * Worker `e`'s look at its sleeps alone")
for old, new in [
    ('      held("is dropped");', '      rl.held("is dropped");'),
    ("    checkExpiring(b, now);", "    rl.checkExpiring(b, now);"),
    ('import * as mv from "./sched_sleep_move";\n', 'import * as mv from "./sched_sleep_move";\nimport * as rl from "./sched_sleep_rule";\n'),
    (
        "/** Are sleeps of block `b` on due lists, not fired yet? */",
        "/** The deadline of sleep `f` (nanoseconds on `clock`). */\n@inline\nexport function deadline(f: usize): u64 {\n  return m.ld(f + S_DEADLINE) as u64;\n}\n\n/** Are sleeps of block `b` on due lists, not fired yet? */",
    ),
]:
    assert s.count(old) == 1, old
    s = s.replace(old, new)
open(P, "w").write(s)
print("moved")
