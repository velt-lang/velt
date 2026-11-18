# cg_crates.py <cachegrind.out>...: self instructions per crate or component (the leading path
# of each function name; known libraries grouped), as shares of the program total.
import collections
import re
import sys

GROUPS = [
    ("alloc (mimalloc, RawVec)", r"^(mi_|_mi_|<alloc::raw_vec|alloc::raw_vec|__rust_(alloc|dealloc|realloc))"),
    ("memcpy/memset/memcmp", r"(memcpy|memmove|memset|memcmp|bcmp)"),
    ("sha2", r"^(sha2::|<sha2::)"),
    ("blake3", r"blake3"),
    ("cranelift", r"cranelift|regalloc2|<regalloc2"),
    ("gimli (debug info)", r"gimli"),
    ("hashbrown/hash", r"hashbrown|core::hash::sip|rustc_hash|FxHasher|foldhash"),
]


def group(name):
    for g, rx in GROUPS:
        if re.search(rx, name):
            return g
    m = re.match(r"^<?(?:&mut )?<?([a-z_][a-z0-9_]*)::(?:([a-z_][a-z0-9_]*)::)?", name)
    if m:
        top = m.group(1)
        if top in ("velt_sema", "velt_vir", "velt_opt", "velt_codegen_cl", "veltc", "velt_loader", "velt_syntax") and m.group(2):
            return f"{top}::{m.group(2)}"
        return top
    return "other"


for path in sys.argv[1:]:
    total = 0
    by = collections.Counter()
    fn = None
    for line in open(path, errors="replace"):
        if line.startswith("fn="):
            fn = line[3:].strip()
            continue
        if line.startswith("summary:") or line.startswith("totals:"):
            total = int(line.split()[1])
            continue
        if fn and line[:1].isdigit():
            parts = line.split()
            if len(parts) >= 2:
                by[group(fn)] += int(parts[1])
    if not total:
        total = sum(by.values())
    print(f"== {path.rsplit('/', 1)[-1]}: {total / 1e9:.3f} G")
    for g, n in by.most_common(28):
        print(f"{100 * n / total:6.1f}%  {n / 1e6:9.1f} M  {g}")
