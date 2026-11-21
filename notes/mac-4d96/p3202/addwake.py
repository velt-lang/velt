import sys
p = sys.argv[1]
s = open(p).read()
old = '''        s = s.replace(old, wide + "  // With the inbox, a request to retire")
'''
new = old + '''        # A spare with nothing to wait for wakes every millisecond (a spurious wake-up, simulated).
        old = "  io.park(b, timeout, e + t.E_PARK, e + t.E_UNPARKS);\\n"
        assert s.count(old) == 1
        s = s.replace(old, "  if (e !== m.ld(e + t.E_MAIN) && timeout < 0) {\\n    timeout = 1;\\n  }\\n" + old)
'''
assert s.count(old) == 1
open(p, "w").write(s.replace(old, new))
