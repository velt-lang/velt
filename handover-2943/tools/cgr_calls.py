# cgr_calls.py <callgrind.out> <callee-regex> [N]: from callgrind's raw output, the number of
# calls into functions matching callee-regex, in total and by caller (exact counts: `calls=`
# lines), with the callee's inclusive cost as callgrind gives it per call edge.
import collections
import re
import sys

path, rx = sys.argv[1], re.compile(sys.argv[2])
top = int(sys.argv[3]) if len(sys.argv) > 3 else 30
names = {}  # compressed id -> name, for fn= / cfn=


def name_of(spec):
    # "(12) name" defines id 12; "(12)" refers to it.
    m = re.match(r"\((\d+)\)(?: (.*))?$", spec.strip())
    if not m:
        return spec.strip()
    i, n = m.group(1), m.group(2)
    if n is not None:
        names[i] = n
    return names.get(i, f"#{i}")


calls = collections.Counter()
cost = collections.Counter()
fn = cfn = None
pending = None
with open(path, errors="replace") as f:
    for line in f:
        if line.startswith("fn="):
            fn = name_of(line[3:])
        elif line.startswith("cfn="):
            cfn = name_of(line[4:])
        elif line.startswith("calls="):
            pending = int(line[6:].split()[0])
        elif pending is not None:
            # The line after `calls=` is the call's position and inclusive cost.
            parts = line.split()
            if cfn and rx.search(cfn):
                calls[fn] += pending
                if len(parts) >= 2 and parts[1].isdigit():
                    cost[fn] += int(parts[1])
            pending = None
total = sum(calls.values())
print(f"calls into /{sys.argv[2]}/: {total:,} (from {len(calls)} callers)")
for caller, n in calls.most_common(top):
    print(f"{n:12,}  {cost[caller] / 1e6:9.1f} M  {caller[:150]}")
