# dhat_top.py <dhat.json> [N] [depth]: allocation sites by number of blocks, each named by its
# first `depth` frames that are velt code (allocator, alloc::, core:: and std:: frames skipped),
# with blocks, bytes and the share of all blocks. Sites with the same frames are summed.
import collections
import json
import re
import sys

d = json.load(open(sys.argv[1]))
top = int(sys.argv[2]) if len(sys.argv) > 2 else 40
depth = int(sys.argv[3]) if len(sys.argv) > 3 else 2
ftbl = d["ftbl"]
SKIP = re.compile(r"(\?\?\?|malloc|calloc|realloc|__rust|alloc::|core::|std::|hashbrown::|<alloc::|<core::|<std::|<hashbrown::|smallvec::|<smallvec::)")


def name(i):
    f = ftbl[i]
    # "0x...: function (file:line)"
    m = re.match(r"^0x[0-9A-Fa-f]+: (.*?)(?: \((.*)\))?$", f)
    fn, loc = (m.group(1), m.group(2) or "") if m else (f, "")
    fn = re.sub(r"::h[0-9a-f]{16}$", "", fn)
    return fn, loc


def velt(loc):
    # A frame of velt's own code: a file under a crates/ directory of the workspace (not the
    # standard library, /rustc/..., nor a dependency, .cargo/registry/...).
    return "/crates/" in loc and "/rustc/" not in loc and ".cargo/registry" not in loc


blocks = collections.Counter()
bytes_ = collections.Counter()
total = 0
for pp in d["pps"]:
    tbk, tb = pp["tbk"], pp["tb"]
    total += tbk
    frames = []
    for i in pp["fs"]:
        fn, loc = name(i)
        if not velt(loc):
            continue
        short = loc.split("/crates/", 1)[-1]
        if depth == 0:
            # By file (depth 0): the first velt frame's file, its line dropped.
            frames.append(short.rsplit(":", 1)[0])
            break
        frames.append(f"{fn} [{short}]")
        if len(frames) >= depth:
            break
    key = " <- ".join(frames) or "(no velt frame)"
    blocks[key] += tbk
    bytes_[key] += tb
print(f"total blocks {total:,}")
for k, n in blocks.most_common(top):
    print(f"{100 * n / total:5.1f}%  {n:10,}  {bytes_[k] / 1e6:8.1f} MB  {k[:230]}")
