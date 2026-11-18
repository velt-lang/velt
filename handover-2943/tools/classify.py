# classify.py <gold_disk.txt>: per failing golden file, the kind of failure and its first
# compiler error / panic line; then counts per first-error message (names masked).
import re
import sys
from collections import Counter, OrderedDict

lines = open(sys.argv[1], errors='replace').read().split('\n')
heads = [(i, l) for i, l in enumerate(lines) if re.match(r'^tests/golden\S+ \[\w+\]: ', l)]
files = OrderedDict()
for n, (i, l) in enumerate(heads):
    end = heads[n + 1][0] if n + 1 < len(heads) else len(lines)
    name, kind = re.match(r'^(tests/golden\S+) \[\w+\]: (.*)$', l).groups()
    first = ''
    for b in lines[i + 1:end]:
        m = re.search(r'(error: .*|ICE: .*|panicked at .*)', b)
        if m and not b.startswith('velt-units'):
            first = m.group(1)
            break
    files.setdefault(name, (kind, first))
kinds = Counter()
msgs = Counter()
for name, (kind, first) in files.items():
    k = re.sub(r'\d+', 'N', kind)
    kinds[k] += 1
    msgs[re.sub(r'`[^`]*`', 'X', first)[:110]] += 1
print('files', len(files))
for k, c in kinds.most_common():
    print(f'{c:4} {k}')
print()
for k, c in msgs.most_common(30):
    print(f'{c:4} {k}')
if len(sys.argv) > 2:
    pat = re.compile(sys.argv[2])
    for name, (kind, first) in files.items():
        if pat.search(first) or pat.search(kind):
            print(name, '|', kind, '|', first[:120])
