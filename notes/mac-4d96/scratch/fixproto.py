import sys
p = sys.argv[1]
s = open(p).read()
old = '''  if (m.load(k + t.T_REFS) === 1) {
    mk.unlockJoin(k, s, kind);
    wk.release(k);
    return;
  }
  m.dec(k + t.T_REFS);
  wk.wake(k);
  mk.unlockJoin(k, s, kind);
}'''
new = '''  const last = m.dec(k + t.T_REFS) === 1;
  if (!last) {
    wk.wake(k);
  }
  mk.unlockJoin(k, s, kind);
  if (last) {
    wk.freed(k);
  }
}'''
assert s.count(old) == 1
s = s.replace(old, new)
old = '''            "/** Waits until the join slot's lock of task `t`, about to be freed, is given back. */\\n"'''
new = '''            "/** Frees task `t`, whose last reference the caller gave back by its own count (`release`'s end). */\\n"
            "export function freed(t: usize): void {\\n"
            "  if ((m.ld32(t + tk.T_FLAGS) & nc.T_PANICKED) !== 0) {\\n"
            "    freePanicked(t);\\n"
            "  }\\n"
            "  m.release(t, m.ld(t + tk.T_BYTES));\\n"
            "}\\n\\n"
            "/** Waits until the join slot's lock of task `t`, about to be freed, is given back. */\\n"'''
assert s.count(old) == 1
s = s.replace(old, new)
open(p, "w").write(s)
