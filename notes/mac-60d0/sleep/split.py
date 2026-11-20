"""#3207: release/isGone/free move from sched_sleep.velt to sched_sleep_move.velt (the 400-line
rule); sched_sleep.velt exports the layout and the two list operations they use."""
H = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/"
s = open(H + "sched_sleep.velt").read()
a = s.index("/**\n * On worker `e`, which holds sleep `f` (`S_HOME`)")
b = s.index("/** Are sleeps of block `b` on due lists, not fired yet? */")
moved = s[a:b]
s = s[:a] + s[b:]


def edit(text, old, new, count=1):
    assert text.count(old) == count, (old, text.count(old))
    return text.replace(old, new)


for name in ("S_WAKER", "S_STATE", "S_TASK", "S_SIZE", "NEW", "PENDING", "DUE", "FIRED", "SEEN", "CARRIED", "GONE"):
    s = edit(s, f"\nconst {name}: usize = ", f"\nexport const {name}: usize = ")
s = edit(s, "\nfunction unlinkDue(k: usize, f: usize): void {", "\nexport function unlinkDue(k: usize, f: usize): void {")
s = edit(s, "\nfunction seen(f: usize): void {", "\nexport function seen(f: usize): void {")
open(H + "sched_sleep.velt", "w").write(s)

for old, new in (
    ("export function release(e: usize, f: usize, drop: bool): u32 {", "function release(e: usize, f: usize, drop: bool): u32 {"),
    ("export function isGone(f: usize): bool {", "function isGone(f: usize): bool {"),
    ("export function free(f: usize): void {", "function free(f: usize): void {"),
    ("(sched_sleep_move.velt). 0:", "(`one`). 0:"),
):
    moved = edit(moved, old, new)
for name in ("S_STATE", "S_HOME", "S_TASK", "S_WAKER", "S_SIZE", "CARRIED", "PENDING", "DUE", "FIRED", "GONE", "SEEN", "NEW"):
    import re

    moved = re.sub(r"(?<![`A-Za-z_.])%s\b" % name, "sl." + name, moved)
for fn in ("unlinkPending(", "unlinkDue(", "seen(", "block("):
    moved = re.sub(r"(?<![A-Za-z_.`])" + re.escape(fn), "sl." + fn, moved)
mv = open(H + "sched_sleep_move.velt").read()
mv = edit(mv, "sl.release(e, f, waker === 0)", "release(e, f, waker === 0)")
mv = edit(mv, "sl.isGone(f)", "isGone(f)")
mv = edit(mv, "sl.free(f)", "free(f)")
mv = edit(mv, "(sched_sleep.velt\n//   `release`)", "(`release`)")
mv = mv.rstrip("\n") + "\n\n" + moved.rstrip("\n") + "\n"
open(H + "sched_sleep_move.velt", "w").write(mv)
print("split")
