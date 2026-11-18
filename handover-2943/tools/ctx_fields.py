# ctx_fields.py <worktree>: the fields of sema's `Ctx` (ctx/mod.rs) missing from the unit codec's
# lists (units/codec/unit.rs `unit_fields!`: save, special, skip), and listed names Ctx lacks.
import re
import sys

root = sys.argv[1] + "/crates/velt_sema/src"
src = open(root + "/ctx/mod.rs").read()
m = re.search(r"pub\(crate\) struct Ctx<'m> \{(.*?)\n\}", src, re.S)
body = m.group(1)
fields = set(re.findall(r"^\s+pub(?:\([^)]*\))? ([a-z_][a-z0-9_]*):", body, re.M))
fields |= set(re.findall(r"^\s{4}([a-z_][a-z0-9_]*):", body, re.M))
unit = open(root + "/units/codec/unit.rs").read()
u = re.search(r"unit_fields! \{(.*?)\n\}", unit, re.S).group(1)
u = re.sub(r"//[^\n]*", "", u)
listed = set(re.findall(r"\b([a-z_][a-z0-9_]*)\b", u)) - {"save", "special", "skip"}
print("missing from the codec:", sorted(fields - listed))
print("listed but not in Ctx:", sorted(n for n in listed - fields if len(n) > 1))
