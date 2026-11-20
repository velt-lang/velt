"""Instrumented copies of a std tree for #3207 (scratch only): each panics with an ICE at one
kind of break of 'a pending sleep is touched only on the worker whose block holds it'.
  exp:  a sleep expires on a worker that is not its task's home (the reviewer's check)
  poll: a pending or due sleep is polled on another worker than its block's
  drop: a pending, due or fired sleep is dropped on another worker than its block's
  python3 mkinstr.py <std-dir> <out-prefix>"""
import shutil
import sys

src, out = sys.argv[1], sys.argv[2]


def edit(s, old, new, count=1):
    assert s.count(old) == count, (old, s.count(old))
    return s.replace(old, new)


for kind in ("exp", "poll", "drop"):
    dst = f"{out}-{kind}"
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst)
    f = dst + "/lang/hooks/sched_sleep.velt"
    s = open(f).read()
    # The block a sleep is linked in, recorded where it is inserted.
    s = edit(s, "const S_SIZE: usize = 80;", "const S_BLOCK: usize = 80;\nconst S_SIZE: usize = 88;")
    s = edit(s, "  m.st(f + S_OWNER, 0);\n  return f;", "  m.st(f + S_OWNER, 0);\n  m.st(f + S_BLOCK, 0);\n  return f;")
    s = edit(
        s,
        "export function insert(b: usize, f: usize): void {\n",
        "export function insert(b: usize, f: usize): void {\n  m.st(f + S_BLOCK, b);\n",
    )
    if kind == "exp":
        s = edit(
            s,
            "      pushDue(k, f);\n",
            "      if (m.ld(k + t.T_HOME) !== t.exec()) {\n"
            '        panic("ICE: probe: exp: a sleep expires off its task\'s home");\n'
            "      }\n      pushDue(k, f);\n",
        )
    if kind == "poll":
        s = edit(
            s,
            "  const w = m.ld(cx + 8);\n  const old = m.ld(f + S_WAKER);\n",
            "  if (s !== NEW && m.ld(f + S_BLOCK) !== m.ld(t.exec() + t.E_TIMERS)) {\n"
            '    panic("ICE: probe: poll: a sleep is polled off its block\'s worker");\n'
            "  }\n  const w = m.ld(cx + 8);\n  const old = m.ld(f + S_WAKER);\n",
        )
    if kind == "drop":
        s = edit(
            s,
            "function sleepDrop(f: usize): void {\n  const s = m.ld(f + S_STATE);\n",
            "function sleepDrop(f: usize): void {\n  const s = m.ld(f + S_STATE);\n"
            "  if (s !== NEW && s !== SEEN && m.ld(f + S_BLOCK) !== m.ld(t.exec() + t.E_TIMERS)) {\n"
            '    panic("ICE: probe: drop: a sleep is dropped off its block\'s worker");\n'
            "  }\n",
        )
    open(f, "w").write(s)
    print(dst)
