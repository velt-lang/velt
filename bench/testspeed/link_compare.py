#!/usr/bin/env python3
"""A golden's compile with each linker `cc` can pick: CPU seconds of the whole build and the link
stage, on PATHs with GNU ld only, with ld.lld, with mold.
usage: link_compare.py <velt> <std> <goldens> <work> <every_n> <mode> [mold dir]"""
import os, re, resource, subprocess, sys
velt, std, gold, work, step, mode = sys.argv[1:7]
mold = sys.argv[7] if len(sys.argv) > 7 else None
step = int(step)
os.makedirs(work, exist_ok=True)
gnu = os.path.join(work, "gnu-bin")
os.makedirs(gnu, exist_ok=True)
for tool in ["cc", "gcc", "ld", "ld.bfd", "as", "clang", "strip", "ar", "nm", "objcopy"]:
    link = os.path.join(gnu, tool)
    if not os.path.lexists(link) and os.path.exists("/usr/bin/" + tool):
        os.symlink("/usr/bin/" + tool, link)
lldbin = os.path.join(work, "lld-bin")
os.makedirs(lldbin, exist_ok=True)
if not os.path.lexists(os.path.join(lldbin, "ld.lld")):
    os.symlink("/usr/bin/ld.lld", os.path.join(lldbin, "ld.lld"))
configs = [("GNU ld", gnu), ("lld", f"{lldbin}:{gnu}")]
if mold:
    configs.append(("mold", f"{mold}:{gnu}"))
files = []
for top, dirs, names in os.walk(gold):
    dirs[:] = sorted(d for d in dirs if not d.startswith("_"))
    for n in sorted(names):
        p = os.path.join(top, n)
        if n.endswith(".velt") and not n.startswith("_") and n != "package.velt" and os.path.exists(p[:-5] + ".out"):
            files.append(p)
files.sort()
sample = files[::step]
print(f"{len(sample)} programs, {mode}")
for round in range(2):
    for name, path in configs:
        cpu = 0.0; link = 0.0; n = 0
        for f in sample:
            env = dict(os.environ, PATH=path, VELT_STD=std, VELT_BUILD_STAMP="off", VELT_BUILD_CACHE=os.path.join(work, "cache"))
            r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
            o = subprocess.run([velt, "build", "--timings", "-o", os.path.join(work, "p")] + (["--release"] if mode == "release" else []) + [f], env=env, capture_output=True, text=True, cwd=work)
            r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
            if o.returncode:
                continue
            n += 1
            cpu += r1.ru_utime + r1.ru_stime - r0.ru_utime - r0.ru_stime
            m = re.search(r"^velt: link\s+([\d.]+) ms", o.stderr, re.M)
            link += float(m.group(1)) if m else 0
        print(f"round {round} {name:8s}: {n} built, CPU {cpu/n:.2f} s/program, link stage {link/n:.0f} ms wall/program", flush=True)
