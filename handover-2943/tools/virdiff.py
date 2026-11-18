# virdiff.py <a.vir> <b.vir> [name-regex]: functions present in only one VIR, and functions whose
# bodies differ (ids normalized), filtered by name-regex.
import re
import sys


def fns(path):
    out = {}
    cur = None
    buf = []
    for line in open(path):
        m = re.match(r'^fn#\d+ \S+ (\S+)\(', line)
        if m:
            if cur:
                out[cur] = buf
            cur = m.group(1)
            buf = []
        elif cur:
            buf.append(re.sub(r'fn#\d+|static#\d+|agg#\d+|ext#\d+|_\d+|bb\d+', '#', line))
    if cur:
        out[cur] = buf
    return out


a = fns(sys.argv[1])
b = fns(sys.argv[2])
flt = re.compile(sys.argv[3]) if len(sys.argv) > 3 else re.compile('.')
only_a = sorted(n for n in a if n not in b and flt.search(n))
only_b = sorted(n for n in b if n not in a and flt.search(n))
diff = sorted(n for n in a if n in b and a[n] != b[n] and flt.search(n))
print('only in first:', len(only_a))
for n in only_a[:40]:
    print('  ', n)
print('only in second:', len(only_b))
for n in only_b[:20]:
    print('  ', n)
print('differing:', len(diff))
for n in diff[:40]:
    print('  ', n)
