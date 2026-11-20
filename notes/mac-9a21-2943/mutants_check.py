"""Checks that every break of mutants.py applies (exactly one match) to the files of
branches/units-u1a as they are checked out there."""
import importlib.util, pathlib, sys

here = pathlib.Path(__file__).parent
src = (here / sys.argv[1] if len(sys.argv) > 1 else here / "mutants.py").read_text().split("env = dict(")[0]
ns = {}
exec(compile(src, "mutants-head", "exec"), ns)
W = pathlib.Path("/Users/andii/dev/velt/branches/units-u1a")
for name, rel, old, new in ns["MUTANTS"]:
    n = (W / rel).read_text().count(old)
    print("%s  %s" % ("ok " if n == 1 else "%d !" % n, name))
