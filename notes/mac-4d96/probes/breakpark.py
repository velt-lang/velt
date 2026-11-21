"""Takes the serve out of park's bail branch (a broken std for the break check)."""
import sys

p = sys.argv[1]
s = open(p).read()
old = "      mv.serve(e);\n    }\n    ex.takeExit(e);"
assert s.count(old) == 1
open(p, "w").write(s.replace(old, "    }\n    ex.takeExit(e);"))
