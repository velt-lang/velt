"""Replace exactly one occurrence of OLD with NEW in FILE (for break checks)."""
import sys

path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path) as f:
    text = f.read()
n = text.count(old)
if n != 1:
    sys.exit(f"swap: {old!r} occurs {n} times in {path}")
with open(path, "w") as f:
    f.write(text.replace(old, new))
