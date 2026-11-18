# intrinsic_codec.py <worktree>: the variants of hir's `Intrinsic` missing from the unit codec's
# `Intrinsic { n => Variant, ... }` list (units/codec/hir.rs), printed as new entries numbered
# after the highest one, with each variant's fields as the enum declares them (f0, f1, ...).
import re
import sys

root = sys.argv[1] + "/crates/velt_sema/src"
import glob

decl = None
for path in glob.glob(root + "/hir/*.rs") + [root + "/hir.rs"]:
    try:
        s = open(path).read()
    except OSError:
        continue
    m = re.search(r"pub enum Intrinsic \{(.*?)\n\}", s, re.S)
    if m:
        decl = m.group(1)
        break
body = re.sub(r"//[^\n]*", "", decl)
variants = {}
for m in re.finditer(r"^\s{4}([A-Z][A-Za-z0-9]*)(\(([^)]*)\))?\s*,", body, re.M):
    n_fields = len([x for x in (m.group(3) or "").split(",") if x.strip()])
    variants[m.group(1)] = n_fields
codec = open(root + "/units/codec/hir.rs").read()
cm = re.search(r"\n    Intrinsic \{(.*?)\n    \}", codec, re.S)
listed = set(re.findall(r"=> ([A-Z][A-Za-z0-9]*)", cm.group(1)))
nums = [int(x) for x in re.findall(r"(\d+) =>", cm.group(1))]
nxt = max(nums) + 1
for v, k in variants.items():
    if v not in listed:
        fields = "(" + ", ".join(f"f{i}" for i in range(k)) + ")" if k else ""
        print(f"        {nxt} => {v}{fields},")
        nxt += 1
