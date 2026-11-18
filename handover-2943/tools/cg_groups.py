# cg_groups.py <self-a.txt> <self-b.txt>: callgrind_annotate self costs (default output) of two
# runs summed by group (the first pattern a function matches), a vs b, sorted by b - a.
import re
import sys

LINE = re.compile(r"^\s*([\d,]+) \(\s*[\d.]+%\)\s+(.+)$")
GROUPS = [
    ("sema units codec decode", r"velt_sema::units::codec|units::codec::Codec>::dec|as velt_sema::units::codec"),
    ("syntax codec decode", r"velt_syntax::codec"),
    ("units reach / live walk", r"units::reach|lazy_std::live|units_live"),
    ("readonly erase", r"readonly::erase"),
    ("generated demand", r"generated::demand"),
    ("module_state", r"module_state"),
    ("loader", r"velt_loader"),
    ("drop glue", r"drop_glue|drop_in_place"),
    ("malloc/free", r"malloc|free|mi_|realloc|calloc|_int_|tcache"),
    ("memcpy/memmove/memset", r"memcpy|memmove|memset|memcmp|bcmp"),
    ("hash maps", r"hashbrown|FxHasher|rustc_hash"),
    ("velt_sema other", r"velt_sema"),
    ("velt_vir", r"velt_vir"),
    ("velt_codegen_cl / cranelift", r"velt_codegen_cl|cranelift"),
    ("velt_syntax other", r"velt_syntax"),
    ("vpm / build cache", r"vpm::|velt_build_cache|blake3|sha2"),
    ("veltc", r"veltc"),
]


def load(path):
    sums = {}
    for line in open(path, errors="replace"):
        if line.startswith("-- Auto-annotated") or line.startswith("-- User-annotated"):
            break
        m = LINE.match(line)
        if not m or "PROGRAM TOTALS" in line:
            continue
        n = int(m.group(1).replace(",", ""))
        name = m.group(2)
        g = next((g for g, p in GROUPS if re.search(p, name)), "other")
        sums[g] = sums.get(g, 0) + n
    return sums


a, b = load(sys.argv[1]), load(sys.argv[2])
rows = sorted(((b.get(g, 0) - a.get(g, 0), g) for g in set(a) | set(b)), reverse=True)
print(f"{'b - a':>9} {'a':>9} {'b':>9}  group")
for d, g in rows:
    print(f"{d / 1e6:8.1f}M {a.get(g, 0) / 1e6:8.1f}M {b.get(g, 0) / 1e6:8.1f}M  {g}")
print(f"{(sum(b.values()) - sum(a.values())) / 1e6:8.1f}M {sum(a.values()) / 1e6:8.1f}M {sum(b.values()) / 1e6:8.1f}M  total")
