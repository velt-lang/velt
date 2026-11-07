#!/usr/bin/env python3
"""Compare two compilers on what a program costs to build (issue #2424).
usage: ir_budget.py lines <velt> <std> <outdir> <file>...         release IR lines, stored as .ll
       ir_budget.py instructions <velt> <std> <file>...           cachegrind, children traced
       ir_budget.py cpu <velt> <std> <file>...                    CPU seconds and binary size
Each run has an empty VELT_HOME and the build cache off. Give both sides the same files."""
import concurrent.futures as cf, glob, os, resource, shutil, subprocess, sys, tempfile

kind, velt, std = sys.argv[1:4]


def env_for(home):
    return dict(os.environ, VELT_HOME=home, VELT_BUILD_CACHE="off", VELT_BUILD_STAMP="off", VELT_STD=std)


def modes(f, out):
    return (("check", ["check", f]), ("debug", ["build", f, "-o", out]), ("release", ["build", "--release", f, "-o", out]))


if kind == "lines":
    outdir = sys.argv[4]
    os.makedirs(outdir, exist_ok=True)

    def one(f):
        name = os.path.basename(f) + ".ll"
        with open(os.path.join(outdir, name), "wb") as h:
            subprocess.run([velt, "build", "--release", "--emit", "llvm", f], stdout=h, stderr=subprocess.DEVNULL, env=env_for(outdir + "/home"), cwd=os.path.dirname(os.path.abspath(f)))
        return f, sum(1 for _ in open(os.path.join(outdir, name), "rb"))

    with cf.ThreadPoolExecutor(4) as ex:
        results = list(ex.map(one, sys.argv[5:]))
    for f, n in sorted(results, key=lambda x: -x[1]):
        print(n, os.path.basename(f))
    print("total", sum(n for _, n in results))
else:
    for f in sys.argv[4:]:
        for mode, args in modes(f, "a.out"):
            d = tempfile.mkdtemp()
            cwd = os.path.dirname(os.path.abspath(f))
            args = [a if a != "a.out" else d + "/a.out" for a in args]
            if kind == "instructions":
                cmd = ["valgrind", "--tool=cachegrind", "--cache-sim=no", "--trace-children=yes", f"--cachegrind-out-file={d}/cg.%p", "--log-file=/dev/null", velt] + args
            else:
                cmd = [velt] + args
            r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
            p = subprocess.run(cmd, env=env_for(d + "/home"), capture_output=True, cwd=cwd)
            r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
            if kind == "instructions":
                total = 0
                for c in glob.glob(d + "/cg.*"):
                    total += sum(int(line.split()[1]) for line in open(c, errors="replace") if line.startswith("summary:"))
                value = str(total)
            else:
                value = f"{r1.ru_utime + r1.ru_stime - r0.ru_utime - r0.ru_stime:.2f}s"
            size = os.path.getsize(d + "/a.out") if os.path.exists(d + "/a.out") else 0
            print(os.path.basename(f), mode, f"rc={p.returncode}", value, size, flush=True)
            shutil.rmtree(d, ignore_errors=True)
