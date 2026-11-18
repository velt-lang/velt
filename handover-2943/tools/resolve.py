# resolve.py <file> <choices>: resolve the merge conflicts in <file> hunk by hunk, keeping every
# line outside them (what git merged cleanly). <choices> has one letter per hunk, in order:
# o = ours (HEAD), t = theirs (origin/main), b = both (ours, then theirs), r = both (theirs,
# then ours). Refuses when the number of letters is not the number of hunks.
import sys

path, choices = sys.argv[1], sys.argv[2]
lines = open(path).read().split("\n")
out, i, hunk = [], 0, 0
ours, theirs, state = [], [], None
for line in lines:
    if line.startswith("<<<<<<< "):
        state, ours, theirs = "ours", [], []
        continue
    if state == "ours" and line.startswith("=======") and line.strip() == "=======":
        state = "theirs"
        continue
    if state == "theirs" and line.startswith(">>>>>>> "):
        if hunk >= len(choices):
            sys.exit(f"{path}: more hunks than choices ({len(choices)})")
        c = choices[hunk]
        out += {"o": ours, "t": theirs, "b": ours + theirs, "r": theirs + ours}[c]
        hunk += 1
        state = None
        continue
    if state == "ours":
        ours.append(line)
    elif state == "theirs":
        theirs.append(line)
    else:
        out.append(line)
if state is not None:
    sys.exit(f"{path}: unterminated conflict")
if hunk != len(choices):
    sys.exit(f"{path}: {hunk} hunks, {len(choices)} choices")
open(path, "w").write("\n".join(out))
print(f"{path}: {hunk} hunks resolved ({choices})")
