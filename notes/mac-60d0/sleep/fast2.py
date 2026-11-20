import sys
p = sys.argv[1]
s = open(p).read()
fast = "      return mv.ask(f, w);\n    }\n"
assert s.count(fast) == 1
s = s.replace(fast, fast + "    if (m.ld(t.exec() + t.E_CURRENT) !== m.ld(f + S_OWNER)) {\n      panic(\"ICE: probe: fast2: the context names the owner, the worker polls another task\");\n    }\n")
open(p, "w").write(s)
