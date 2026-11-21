"""Scratch stds for the retire-request race (#3202): cross-wake's std with the window widened (a
spare waits ~300 us with its park word down just before it raises it, so a request stored then
finds the word down), with the fix (`fix`) and without it (`broken`).
  python3 mk.py <out-dir>"""
import shutil
import sys

src = "/Users/andii/dev/velt/branches/cross-wake/std"
out = sys.argv[1]
fixed = (
    "  const asked = (m.load32(e + t.E_RETIRED) & t.R_AWAY) as usize;\n"
    "  if ((m.load(e + t.E_INBOX) | asked) !== 0 || q.any(e) || !parkable(e)) {"
)
broken = "  if (m.load(e + t.E_INBOX) !== 0 || q.any(e) || !parkable(e)) {"
wide = (
    "  if (e !== m.ld(e + t.E_MAIN) && m.load32(e + t.E_RETIRED) === 0) {\n"
    "    const until = (sl.clock() as usize) + 300000;\n"
    "    while ((sl.clock() as usize) < until) {}\n"
    "  }\n"
    "  m.store32(e + t.E_PARK, 1);\n"
)
for name in ("fix", "broken", "fix-plain", "broken-plain"):
    dst = f"{out}/std-{name}"
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst)
    p = dst + "/lang/hooks/sched_park.velt"
    s = open(p).read()
    assert s.count(fixed) == 1
    if name.startswith("broken"):
        s = s.replace(fixed, broken)
    if not name.endswith("plain"):
        old = "  m.store32(e + t.E_PARK, 1);\n  // With the inbox, a request to retire"
        assert s.count(old) == 1
        s = s.replace(old, wide + "  // With the inbox, a request to retire")
        # A spare with nothing to wait for wakes every millisecond (a spurious wake-up, simulated).
        old = "  io.park(b, timeout, e + t.E_PARK, e + t.E_UNPARKS);\n"
        assert s.count(old) == 1
        s = s.replace(old, "  if (e !== m.ld(e + t.E_MAIN) && timeout < 0) {\n    timeout = 1;\n  }\n" + old)
    open(p, "w").write(s)
    print(dst)
