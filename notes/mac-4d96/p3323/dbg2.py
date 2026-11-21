"""Scratch std: stops at the first reference taken on a handed-over, unfinished node, naming the
site: python3 dbg2.py <out-dir>"""
import shutil
import sys

src = "/Users/andii/dev/velt/branches/parked-away/std"
dst = sys.argv[1]
shutil.rmtree(dst, ignore_errors=True)
shutil.copytree(src, dst)


def edit(name, old, new):
    p = dst + "/lang/hooks/" + name
    s = open(p).read()
    assert s.count(old) == 1, (name, old)
    open(p, "w").write(s.replace(old, new))


def probe(var, site, nd=""):
    return (
        f"  if ((m.load32({var} + {nd}N_FLAGS) & (2048 | 2)) === 2048) {{\n"
        f'    panic("ICE: probe: reference taken at {site}");\n'
        "  }\n"
    )


edit("sched_node_wake.velt", "export function nodeClone(w: usize): usize {\n", "export function nodeClone(w: usize): usize {\n" + probe("w", "nodeClone", "nd."))
edit("sched_node_wake.velt", "  m.inc(n + nd.N_REFS);\n  const task = m.ld(n + nd.N_TASK);\n  m.pushStack", probe("n", "wake", "nd.") + "  m.inc(n + nd.N_REFS);\n  const task = m.ld(n + nd.N_TASK);\n  m.pushStack")
