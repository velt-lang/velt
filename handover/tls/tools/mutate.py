#!/usr/bin/env python3
# mutate.py <test file> <mutations file>: each mutation "path|||old|||new" is applied alone; the
# test must fail. Prints KILLED / SURVIVED per mutation and restores the file.
import subprocess, sys, os
test, muts = sys.argv[1], sys.argv[2]
extra = sys.argv[3:]
for line in open(muts):
    line = line.rstrip("\n")
    if not line or line.startswith("#"): continue
    path, old, new = line.split("@@@")
    src = open(path).read()
    old=old.replace("\\n","\n"); new=new.replace("\\n","\n")
    if src.count(old) < 1:
        print("NOT FOUND", path, old); continue
    open(path, "w").write(src.replace(old, new, 1))
    try:
        r = subprocess.run([os.environ.get("VELT_BIN","./target/debug/velt"), "test", *extra, test], capture_output=True, text=True, timeout=900)
        out = r.stdout + r.stderr
        ok = "test result: ok" in out
        print(("SURVIVED " if ok else "KILLED   ") + path.split("/")[-1] + ": " + old[:60].replace("\n"," "))
    finally:
        open(path, "w").write(src)
