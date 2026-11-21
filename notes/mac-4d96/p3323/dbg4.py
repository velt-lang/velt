"""Scratch std: stops when a started, unfinished node with two references gets a third, naming
the site: python3 dbg4.py <out-dir>"""
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


def probe(var, site):
    return (
        f"  if ((m.load32({var} + nd.N_FLAGS) & 3) === 1 && m.load({var} + nd.N_REFS) === 2) {{\n"
        f'    panic("ICE: probe: third reference taken at {site}");\n'
        "  }\n"
    )


edit("sched_node_wake.velt", "export function nodeClone(w: usize): usize {\n", "export function nodeClone(w: usize): usize {\n" + probe("w", "nodeClone"))
edit("sched_node_wake.velt", "  m.inc(n + nd.N_REFS);\n  const task = m.ld(n + nd.N_TASK);\n  m.pushStack", probe("n", "wake") + "  m.inc(n + nd.N_REFS);\n  const task = m.ld(n + nd.N_TASK);\n  m.pushStack")
edit("sched_node_catch.velt", "    m.inc(n + nd.N_REFS);\n", '    panic("ICE: probe: reference taken in node_catch");\n    m.inc(n + nd.N_REFS);\n')
