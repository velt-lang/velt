import sys, re, subprocess, collections
# Hottest instructions of a callgrind run (`valgrind --tool=callgrind --dump-instr=yes`), with
# their disassembly: python3 bench/array_view/hot.py <callgrind.out> <binary> [n]
f, binp = sys.argv[1], sys.argv[2]
n = int(sys.argv[3]) if len(sys.argv) > 3 else 70
cnt = collections.Counter()
npos = 1
last = [0, 0]
skip = False
for line in open(f):
    line = line.rstrip("\n")
    if line.startswith("positions:"):
        npos = len(line.split()[1:])
        continue
    if line.startswith("calls="):
        skip = True
        continue
    if not line or line[0] not in "0123456789+-*":
        continue
    parts = line.split()
    pos = []
    for i in range(npos):
        a = parts[i]
        if a.startswith("0x"):
            v = int(a, 16)
        elif a == "*":
            v = last[i]
        elif a[0] in "+-":
            v = last[i] + int(a)
        else:
            v = int(a)
        pos.append(v)
    last[:npos] = pos
    if skip:
        skip = False
        continue
    cost = int(parts[npos]) if len(parts) > npos else 0
    cnt[pos[0]] += cost
tot = sum(cnt.values())
top = sorted(cnt.items(), key=lambda x: -x[1])[:n]
print("total", tot)
hot = dict(top)
lo = min(hot)
hi = max(hot)
dis = subprocess.run(["objdump", "-d", "--no-show-raw-insn", f"--start-address={lo}",
                      f"--stop-address={hi + 16}", binp], capture_output=True, text=True).stdout
for l in dis.splitlines():
    m = re.match(r"^\s*([0-9a-f]+):\s*(.*)$", l)
    if m:
        a = int(m.group(1), 16)
        if a in hot:
            print(f"{hot[a]:>10} {a:x}: {m.group(2)}")
