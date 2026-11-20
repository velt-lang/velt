"""#3207: the program's end waits for the sleep requests under way (a counted exit takes its
counts only after every request, its waker and any dropped sleep were given back)."""
H = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/"


def edit(name, pairs):
    s = open(H + name).read()
    for old, new in pairs:
        assert s.count(old) == 1, (name, old[:50], s.count(old))
        s = s.replace(old, new)
    open(H + name, "w").write(s)


edit(
    "sched_sleep_move.velt",
    [
        (
            "const R_SIZE: usize = 24;\n",
            "const R_SIZE: usize = 24;\n\n/** The requests made and not yet served, on every worker's list together (`settle`). */\nconst UNDER_WAY = new Atomic<u64>(0);\n",
        ),
        (
            "function post(f: usize, waker: usize): void {\n  m.inc(f + sl.S_ASKS);\n",
            "function post(f: usize, waker: usize): void {\n  UNDER_WAY.add(1);\n  m.inc(f + sl.S_ASKS);\n",
        ),
        (
            "  // This request named the sleep until here: the last one of a dropped sleep frees it.\n  if (m.dec(f + sl.S_ASKS) === 1 && isGone(f)) {\n    free(f);\n  }\n}\n",
            "  // This request named the sleep until here: the last one of a dropped sleep frees it.\n  if (m.dec(f + sl.S_ASKS) === 1 && isGone(f)) {\n    free(f);\n  }\n  UNDER_WAY.sub(1);\n}\n\n"
            "/**\n * The program ends (sched.velt `schedFinish`, on worker 0 `e`, no task left): once every request\n"
            " * under way was served, by this worker for its own list and by the others, which the requests\n"
            " * woke. Each is a few steps; the core is given away meanwhile. A counted exit then finds the\n"
            " * requests, their wakers and the dropped sleeps given back.\n */\n"
            "export function settle(e: usize): void {\n"
            "  while (UNDER_WAY.load() !== 0) {\n"
            "    if (m.load(e + t.E_SLEEP_ASKS) !== 0) {\n      serve(e);\n    } else {\n      m.yieldCore();\n    }\n  }\n}\n",
        ),
        (
            "//   every request is served, its waker given back, before a counted exit takes its counts.\n",
            "//   the program does not end while tasks could still ask; once no task is left, its end waits\n//   for the requests under way (`settle`): every request is served, its waker given back and a\n//   dropped sleep freed before a counted exit takes its counts.\n",
        ),
    ],
)
edit(
    "sched.velt",
    [
        ("  wk.settleWakes();\n  // Streams dropped unclosed", "  wk.settleWakes();\n  mv.settle(e);\n  // Streams dropped unclosed"),
    ],
)
print("settle")
