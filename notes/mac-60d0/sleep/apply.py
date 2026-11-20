"""#3207 on branch sleep-home: the edits to existing std files (the new file is written apart)."""
H = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/"


def edit(name, pairs):
    s = open(H + name).read()
    for old, new in pairs:
        assert s.count(old) == 1, (name, old, s.count(old))
        s = s.replace(old, new)
    open(H + name, "w").write(s)


edit(
    "sched_sleep.velt",
    [
        (
            'import * as m from "./sched_mem";\nimport * as sp from "./sched_spread";\n',
            'import * as m from "./sched_mem";\nimport * as mv from "./sched_sleep_move";\nimport * as sp from "./sched_spread";\n',
        ),
        (
            "export const S_OWNER: usize = 72;\nconst S_SIZE: usize = 80;\n",
            "export const S_OWNER: usize = 72;\n"
            "/**\n * The worker whose timer block holds it, once it was polled there (read from other threads:\n"
            " * sched_sleep_move.velt).\n */\n"
            "export const S_HOME: usize = 80;\n"
            "/** The hand-overs and the drop under way for it, on other workers' lists (sched_sleep_move.velt). */\n"
            "export const S_ASKS: usize = 88;\n"
            "const S_SIZE: usize = 96;\n",
        ),
        (
            "// A sleep's states: not polled yet; pending; due (on its task's due list); fired (counted in\n"
            "// `TM_UNSEEN` until its leaf sees it); seen.\n",
            "// A sleep's states: not polled yet; pending; due (on its task's due list); fired (counted in\n"
            "// `TM_UNSEEN` until its leaf sees it); seen; carried by an offered task (in its `T_TIMERS`,\n"
            "// sched_sleep_carry.velt); dropped while a hand-over was under way (sched_sleep_move.velt).\n",
        ),
        ("const SEEN: usize = 4;\n", "const SEEN: usize = 4;\nconst CARRIED: usize = 5;\nconst GONE: usize = 6;\n"),
        (
            "  m.st(f + S_OWNER, 0);\n  return f;\n",
            "  m.st(f + S_OWNER, 0);\n  m.st(f + S_HOME, 0);\n  m.st(f + S_ASKS, 0);\n  return f;\n",
        ),
        (
            "function sleepPoll(f: usize, cx: usize): u32 {\n  const s = m.ld(f + S_STATE);\n",
            "function sleepPoll(f: usize, cx: usize): u32 {\n  // Loaded as shared: a worker that gave this sleep up stored its state last (`release`).\n  const s = m.load(f + S_STATE);\n"
            "  if (s !== NEW && s !== SEEN && away(f, s)) {\n"
            "    // Held by another worker's block (handed on by the task that polled it there): that\n"
            "    // worker gives it up, and this task polls it again as a new one.\n"
            "    return mv.ask(f, m.ld(cx + 8));\n"
            "  }\n",
        ),
        (
            "    m.st(f + S_WAKER, m.wakerClone(w));\n    if (old !== 0) {\n      m.wakerDrop(old);\n    }\n  }\n",
            "    m.st(f + S_WAKER, m.wakerClone(w));\n    if (old !== 0) {\n      m.wakerDrop(old);\n"
            "      // Polled by another task than the one that polled it into this block: that task\n"
            "      // stays with it from now on (an offer carries it along), as the first did.\n"
            "      sp.anchor();\n"
            "      m.st(f + S_OWNER, m.ld(t.exec() + t.E_CURRENT));\n"
            "    }\n  }\n",
        ),
        (
            "    const e = t.exec();\n    m.st(f + S_OWNER, m.ld(e + t.E_CURRENT));\n    insert(block(e), f);\n",
            "    const e = t.exec();\n    m.st(f + S_OWNER, m.ld(e + t.E_CURRENT));\n    m.storeRelease(f + S_HOME, e);\n    insert(block(e), f);\n",
        ),
        (
            "  m.st(f + S_STATE, PENDING);\n",
            "  // Released: a worker that gave this sleep up reads the state, then `S_HOME` (`release`).\n"
            "  m.storeRelease(f + S_STATE, PENDING);\n",
        ),
        (
            "function sleepDrop(f: usize): void {\n  const s = m.ld(f + S_STATE);\n",
            "function sleepDrop(f: usize): void {\n  const s = m.load(f + S_STATE);\n"
            "  if (m.load(f + S_ASKS) !== 0 || (s !== NEW && s !== SEEN && away(f, s))) {\n"
            "    // Its worker (or the one a hand-over under way reaches) takes it off its list and frees it.\n"
            "    mv.dropAway(f);\n    return;\n  }\n",
        ),
        (
            "/** Fired sleep `f` was seen (polled ready, or dropped). */\n",
            "/**\n * Is sleep `f` (state `s`, polled before) out of this worker's reach: in another worker's block, or\n"
            " * carried by an offered task?\n */\n"
            "@inline\nfunction away(f: usize, s: usize): bool {\n"
            "  return s === CARRIED || m.load(f + S_HOME) !== t.exec();\n}\n\n"
            "/** Fired sleep `f` was seen (polled ready, or dropped). */\n",
        ),
        (
            "/** Sleep `f`, on no list: its waker dropped, freed. */\nfunction gone(f: usize): void {\n",
            "/** Sleep `f`, on no list: its waker dropped, freed. */\nexport function gone(f: usize): void {\n",
        ),
        (
            "/** Are sleeps of block `b` on due lists, not fired yet? */\n",
            "/** Sleep `f` leaves its worker's block for an offered task's list (sched_sleep_carry.velt `carry`). */\n"
            "export function carried(b: usize, f: usize): void {\n  unlinkPending(b, f);\n  m.st(f + S_STATE, CARRIED);\n}\n\n"
            "/**\n * On worker `e`, which holds sleep `f` (`S_HOME`): what a hand-over or a drop from another worker\n"
            " * finds (sched_sleep_move.velt). 0: carried by an offered task, not to be touched yet. 1: off\n"
            " * its list, a new sleep for whoever polls it next (`drop`: its waker dropped, state `GONE`).\n"
            " * 2: it had fired (seen now). 3: another worker holds it by now.\n */\n"
            "export function release(e: usize, f: usize, drop: bool): u32 {\n"
            "  // The state first, then the worker: a task that polls it as a new sleep on another worker\n"
            "  // stores that worker before its state (`sleepPoll`), so a state read as that task's comes\n"
            "  // with its worker, and this one leaves the sleep alone.\n"
            "  const s = m.load(f + S_STATE);\n"
            "  if (m.load(f + S_HOME) !== e) {\n    return 3;\n  }\n"
            "  if (s === CARRIED) {\n    return 0;\n  }\n"
            "  if (s === PENDING) {\n    unlinkPending(block(e), f);\n"
            "  } else if (s === DUE) {\n    unlinkDue(m.ld(f + S_TASK), f);\n"
            "  } else if (s === FIRED) {\n    seen(f);\n"
            "  } else if (!drop) {\n"
            "    // New again (an earlier hand-over), seen or dropped: not this worker's to write; the\n"
            "    // task that asked polls it and finds that.\n"
            "    return s === NEW ? 1 : 2;\n  }\n"
            "  const w = m.ld(f + S_WAKER);\n"
            "  if (w !== 0) {\n    m.st(f + S_WAKER, 0);\n    m.wakerDrop(w);\n  }\n"
            "  // Last: from here on the task that asked may poll it, on its own worker.\n"
            "  m.storeRelease(f + S_STATE, drop ? GONE : s === FIRED ? SEEN : NEW);\n"
            "  return s === FIRED ? 2 : 1;\n}\n\n"
            "/** Was sleep `f` dropped while other workers' lists named it (`GONE`)? */\n"
            "export function isGone(f: usize): bool {\n  return m.load(f + S_STATE) === GONE;\n}\n\n"
            "/** Frees sleep `f`, dropped (`GONE`), once nothing on any worker's list names it. */\n"
            "export function free(f: usize): void {\n  m.release(f, S_SIZE);\n}\n\n"
            "/** Are sleeps of block `b` on due lists, not fired yet? */\n",
        ),
    ],
)
edit(
    "sched_sleep_carry.velt",
    [
        ("      sl.unlinkPending(b, f);\n", "      sl.carried(b, f);\n"),
        ("    sl.insert(b, f);\n", "    m.storeRelease(f + sl.S_HOME, e);\n    sl.insert(b, f);\n"),
    ],
)
edit(
    "sched_task.velt",
    [
        (
            "export const E_LIVE_F: usize = 984;\nexport const E_SIZE: usize = 992;\n",
            "export const E_LIVE_F: usize = 984;\n"
            "/**\n * What other workers ask of this one's sleeps (a stack any thread pushes on, taken whole by the\n"
            " * worker: sched_sleep_move.velt).\n */\n"
            "export const E_SLEEP_ASKS: usize = 992;\nexport const E_SIZE: usize = 1000;\n",
        ),
    ],
)
edit(
    "sched_end.velt",
    [
        (
            "  if (m.load(w + t.E_INBOX) !== 0 || q.any(w) || m.load(w + t.E_OFFER) !== 0) {\n    return true;\n  }\n  return waits(w);",
            "  if (m.load(w + t.E_INBOX) !== 0 || q.any(w) || m.load(w + t.E_OFFER) !== 0) {\n    return true;\n  }\n"
            "  if (m.load(w + t.E_SLEEP_ASKS) !== 0) {\n    // A sleep another worker asked it for or dropped (sched_sleep_move.velt): it serves that.\n    return true;\n  }\n  return waits(w);",
        ),
    ],
)
edit(
    "sched_tick.velt",
    [
        (
            'import * as q from "./sched_queue";\n',
            'import * as q from "./sched_queue";\nimport * as mv from "./sched_sleep_move";\n',
        ),
        (
            "function ioTick(e: usize, b: usize): void {\n",
            "function ioTick(e: usize, b: usize): void {\n  if (m.load(e + t.E_SLEEP_ASKS) !== 0) {\n    mv.serve(e);\n  }\n",
        ),
    ],
)
edit(
    "sched_park.velt",
    [
        (
            'import * as sl from "./sched_sleep";\n',
            'import * as sl from "./sched_sleep";\nimport * as mv from "./sched_sleep_move";\n',
        ),
        (
            "  const b = m.ld(e + t.E_IO);\n  m.st32(e + t.E_DISPATCH, t.D_INPUT);\n",
            "  const b = m.ld(e + t.E_IO);\n  if (m.load(e + t.E_SLEEP_ASKS) !== 0) {\n    // Before its timer look: a sleep it gives up does not expire here.\n    mv.serve(e);\n  }\n  m.st32(e + t.E_DISPATCH, t.D_INPUT);\n",
        ),
        (
            "  if (m.load(e + t.E_INBOX) !== 0 || q.any(e) || !parkable(e)) {\n",
            "  if (\n    m.load(e + t.E_INBOX) !== 0 ||\n    m.load(e + t.E_SLEEP_ASKS) !== 0 ||\n    q.any(e) ||\n    !parkable(e)\n  ) {\n",
        ),
    ],
)
print("applied")
