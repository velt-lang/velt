"""Std trees for #3360: main's (from git, origin/main) and the fix (the settle-race worktree),
each widened: fulfillLater spins 0.3 ms just before its frame ends.  python3 mk.py <dir>"""
import shutil
import subprocess
import sys

out = sys.argv[1]
B = "/Users/andii/dev/velt/branches"
spin = "  const until = performance.now() + 0.3;\n  while (performance.now() < until) {}\n}\n"


def widen(dst, tail):
    p = dst + "/lang/promise/settle.velt"
    s = open(p).read()
    assert s.count(tail) == 1, tail
    open(p, "w").write(s.replace(tail, tail[:-2] + spin))


main = out + "/std-main-wide"
shutil.rmtree(main, ignore_errors=True)
shutil.rmtree(out + "/tmp-main", ignore_errors=True)
subprocess.run(f"mkdir -p {out}/tmp-main && git -C {B}/settle-race archive origin/main std | tar -x -C {out}/tmp-main", shell=True, check=True)
shutil.move(out + "/tmp-main/std", main)
widen(main, '  openLatch(guard.with((g) => slot.with((s) => s.settle({ status: "fulfilled", value: v }))));\n}\n')

fix = out + "/std-fix"
shutil.rmtree(fix, ignore_errors=True)
shutil.copytree(B + "/settle-race/std", fix)
fw = out + "/std-fix-wide"
shutil.rmtree(fw, ignore_errors=True)
shutil.copytree(fix, fw)
widen(fw, "  openLatch(fulfillFrom<T, E>(slot, guard, value));\n}\n")
