#!/usr/bin/env python3
"""Velt against C# on the programs of https://devmobasa.github.io/how-fast-is-csharp/.

    python3 bench/vs-csharp/run.py setup WORK        # fetch the C# sources, build them (NativeAOT)
    python3 bench/vs-csharp/run.py run WORK [--runs 5] [--structures a,b] [--cpu 2] [--velt PATH]
    python3 bench/vs-csharp/run.py cachegrind WORK [--structures a,b] [--scale 10]

`setup` downloads each structure's C# solution and driver from the original benchmark
(ben-chmarks.com, the files the page's bench.py uses) and the page's optimized C# ("C# new",
github.com/devmobasa/how-fast-is-csharp, csharp/), and builds both with the page's settings:
`dotnet publish -c Release -r linux-x64 -p:PublishAot=true -p:OptimizationPreference=Speed
-p:IlcInstructionSet=native` (net11.0). It needs the newest .NET SDK on PATH.

`run` builds the Velt ports (`velt build --release`), then runs C# original, C# new and Velt
interleaved, `--runs` times each, as the page does: `<driver> all <n> 1 1` (one warm-up and one
timed repetition of every case per process), pinned to one CPU with taskset when it exists. Every
run's ops, checksum and verify must equal C# original's. It prints, per structure, each case's
median time and the score (the geometric mean of the case medians, in ms), the median process
wall time and the peak resident memory. Results are printed, never written into the repository.

`cachegrind` counts the instructions of the Velt programs (valgrind --tool=cachegrind) at
n / --scale entries.
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path
from urllib.request import urlopen

HERE = Path(__file__).resolve().parent
SITE = "https://ben-chmarks.com/benchmarks"
MODEL = "gemini-4-neon_high"
PAGE_REPO = "https://github.com/devmobasa/how-fast-is-csharp"
AOT = ["-p:PublishAot=true", "-p:OptimizationPreference=Speed", "-p:IlcInstructionSet=native"]

# Name, C# solution file and the page's entry count of each structure.
STRUCTURES = {
    "btree": ("B-tree", "BTree.cs", 4_000_000),
    "dlist": ("Doubly linked list", "DList.cs", 3_000_000),
    "hashmap": ("Hash map", "HashMap.cs", 2_000_000),
    "heap": ("Binary heap", "Heap.cs", 3_000_000),
    "radix": ("Radix tree", "RadixTree.cs", 1_500_000),
    "rbtree": ("Red-black tree", "RBTree.cs", 1_500_000),
    "rtree": ("R-tree", "RTree.cs", 400_000),
    "skiplist": ("Skip list", "SkipList.cs", 1_250_000),
    "unionfind": ("Union-find", "UnionFind.cs", 4_000_000),
}

PROJECT = """<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net11.0</TargetFramework>
    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>
  </PropertyGroup>
</Project>
"""


def fetch(url):
    with urlopen(url, timeout=60) as response:
        return response.read()


def setup(work):
    work.mkdir(parents=True, exist_ok=True)
    page = work / "how-fast-is-csharp"
    if not page.exists():
        subprocess.run(["git", "clone", "-q", "--depth", "1", PAGE_REPO, page], check=True)
    for key, (_, source, _) in STRUCTURES.items():
        driver = fetch(f"{SITE}/{key}/lang/csharp/Driver.cs")
        original = fetch(f"{SITE}/{key}/solutions/{MODEL}/csharp/{source}")
        for variant, code in (("orig", original), ("new", (page / "csharp" / source).read_bytes())):
            d = work / "cs" / f"{key}-{variant}"
            d.mkdir(parents=True, exist_ok=True)
            (d / source).write_bytes(code)
            (d / "Driver.cs").write_bytes(driver)
            (d / "Bench.csproj").write_text(PROJECT, encoding="utf-8")
            subprocess.run(
                ["dotnet", "publish", "-r", "linux-x64", "-o", d / "native", *AOT, "-c", "Release",
                 "--nologo", "--disable-build-servers", d / "Bench.csproj"],
                stdout=sys.stderr, check=True,
            )
            print(f"built C# {variant} {key}", file=sys.stderr)


def build_velt(work, velt, key, cpu="native", extra=()):
    out = work / "velt" / f"{key}-{cpu}"
    out.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run([velt, "build", "--release", "--cpu", cpu, *extra, HERE / key / "driver.velt", "-o", out], check=True)
    return out


def run_once(command):
    """Runs a driver; returns its JSON rows, wall seconds and peak RSS in MiB."""
    start = time.perf_counter()
    proc = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    out, err = proc.communicate()
    wall = time.perf_counter() - start
    if proc.returncode != 0:
        raise RuntimeError(f"{command} failed ({proc.returncode}):\n{err}")
    rows = [json.loads(line) for line in out.splitlines() if line.strip()]
    return rows, wall


def peak_mib(command):
    """Peak RSS of one run, measured on its own so the other children don't count."""
    pid = os.fork()
    if pid == 0:
        devnull = os.open(os.devnull, os.O_WRONLY)
        os.dup2(devnull, 1)
        os.dup2(devnull, 2)
        os.execvp(str(command[0]), [str(c) for c in command])
    _, status, usage = os.wait4(pid, 0)
    if status != 0:
        raise RuntimeError(f"{command} failed")
    return usage.ru_maxrss / 1024


def timed(rows):
    return {r["case"]: r for r in rows if not r["warmup"]}


def geomean(values):
    product = 1.0
    for v in values:
        product *= v
    return product ** (1 / len(values))


def run(work, args):
    keys = args.structures.split(",") if args.structures else list(STRUCTURES)
    pin = ["taskset", "-c", str(args.cpu)] if shutil.which("taskset") else []
    summary = []
    for key in keys:
        name, _, entries = STRUCTURES[key]
        entries = args.entries or entries
        programs = {
            "C# original": [work / "cs" / f"{key}-orig" / "native" / "Bench"],
            "C# new": [work / "cs" / f"{key}-new" / "native" / "Bench"],
            "Velt": [build_velt(work, args.velt, key)],
        }
        argv = ["all", str(entries), "1", "1"]
        samples = {p: [] for p in programs}
        walls = {p: [] for p in programs}
        reference = None
        for i in range(args.runs):
            order = list(programs) if i % 2 == 0 else list(reversed(programs))
            for p in order:
                rows, wall = run_once([*pin, *programs[p], *argv])
                cases = timed(rows)
                expected = [(c, r["ops"], r["checksum"], r["verify"]) for c, r in cases.items()]
                if reference is None:
                    reference = expected
                elif expected != reference:
                    sys.exit(f"{key}: {p} printed other results than the first program")
                samples[p].append({c: r["ns"] for c, r in cases.items()})
                walls[p].append(wall)
        mem = {p: peak_mib([*programs[p], *argv]) for p in programs}
        cases = [c for c, *_ in reference]
        print(f"\n{name} ({key}), n = {entries:,}, {args.runs} runs (median ms per case)")
        print(f"{'case':<24}" + "".join(f"{p:>14}" for p in programs) + f"{'Velt/C# new':>14}")
        medians = {p: {c: statistics.median(s[c] for s in samples[p]) / 1e6 for c in cases} for p in programs}
        for c in cases:
            line = f"{c:<24}" + "".join(f"{medians[p][c]:>14.2f}" for p in programs)
            print(line + f"{medians['Velt'][c] / medians['C# new'][c]:>14.2f}")
        scores = {p: geomean(medians[p].values()) for p in programs}
        print(f"{'score (geomean ms)':<24}" + "".join(f"{scores[p]:>14.2f}" for p in programs)
              + f"{scores['Velt'] / scores['C# new']:>14.2f}")
        print(f"{'process wall s':<24}" + "".join(f"{statistics.median(walls[p]):>14.2f}" for p in programs))
        print(f"{'peak RSS MiB':<24}" + "".join(f"{mem[p]:>14.0f}" for p in programs))
        summary.append((name, scores, mem))
    print("\nSummary (score ms; ratio Velt / C#)")
    print(f"{'structure':<20}{'C# orig':>10}{'C# new':>10}{'Velt':>10}{'/orig':>8}{'/new':>8}")
    for name, s, _ in summary:
        print(f"{name:<20}{s['C# original']:>10.1f}{s['C# new']:>10.1f}{s['Velt']:>10.1f}"
              f"{s['Velt'] / s['C# original']:>8.2f}{s['Velt'] / s['C# new']:>8.2f}")


def cachegrind(work, args):
    keys = args.structures.split(",") if args.structures else list(STRUCTURES)
    for key in keys:
        entries = max(1, STRUCTURES[key][2] // args.scale)
        # valgrind does not run AVX-512 code: the default CPU level, with debug info for cg_annotate.
        binary = build_velt(work, args.velt, key, cpu="x86-64-v2", extra=["-g"])
        out = work / "cachegrind" / f"{key}.out"
        out.parent.mkdir(parents=True, exist_ok=True)
        proc = subprocess.run(
            ["valgrind", "--tool=cachegrind", "--cache-sim=no", f"--cachegrind-out-file={out}",
             binary, "all", str(entries), "1", "0"],
            capture_output=True, text=True, check=True,
        )
        refs = [line for line in proc.stderr.splitlines() if "I refs" in line or "I   refs" in line]
        print(f"{key:<10} n={entries:<10,} {refs[-1].split(':', 1)[1].strip() if refs else '?'}  ({out})")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["setup", "run", "cachegrind"])
    parser.add_argument("work", type=Path, help="directory for sources, binaries and outputs")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--structures", default="")
    parser.add_argument("--entries", type=int, default=0, help="override every structure's n")
    parser.add_argument("--cpu", type=int, default=2)
    parser.add_argument("--scale", type=int, default=10)
    parser.add_argument("--velt", default=os.environ.get("VELT", "velt"))
    args = parser.parse_args()
    work = args.work.resolve()
    {"setup": lambda: setup(work), "run": lambda: run(work, args), "cachegrind": lambda: cachegrind(work, args)}[args.command]()


if __name__ == "__main__":
    main()
