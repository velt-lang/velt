"""The benchmark showcase: Velt against Rust, Node and Bun on the existing suites under bench/.
Run it through bench/showcase/run.sh (the machine checks are there); README.md has the method.

  showcase.py --out DIR [--smoke] [--runs N] [--categories a,b] [--only substr] [--velt PATH]

Builds everything first (release), then runs every category, then writes DIR/results.json and a
Markdown table per category (report.py).
"""

import argparse
import datetime
import json
import os
import platform
import shutil
import subprocess
import sys
import time

import core
import engine
import report
import servers
import suites

CATEGORIES = ["startup", "cpu", "core", "benchmarks-game", "regex", "async", "json", "tsx", "sqlite",
              "compile", "http", "websocket"]


class Ctx:
    pass


def tool(name, *extra):
    found = shutil.which(name)
    if found:
        return found
    for d in extra:
        p = os.path.expanduser(os.path.join(d, name))
        if os.path.isfile(p) and os.access(p, os.X_OK):
            return p
    return None


def version(cmd):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
        return (p.stdout or p.stderr).strip().splitlines()[0]
    except (OSError, IndexError, subprocess.TimeoutExpired):
        return None


def read(path):
    try:
        return open(path).read().strip()
    except OSError:
        return None


def machine(ctx):
    cpu = None
    for line in (read("/proc/cpuinfo") or "").splitlines():
        if line.startswith("model name"):
            cpu = line.split(":", 1)[1].strip()
            break
    mem = None
    for line in (read("/proc/meminfo") or "").splitlines():
        if line.startswith("MemTotal:"):
            mem = f"{int(line.split()[1]) / 1048576:.1f} GB"
    commit = version(["git", "-C", ctx.root, "rev-parse", "HEAD"])
    dirty = subprocess.run(["git", "-C", ctx.root, "status", "--porcelain", "--untracked-files=no"],
                           capture_output=True, text=True).stdout.strip()
    return {
        "date": ctx.date,
        "host": platform.node(),
        "os": version(["sh", "-c", ". /etc/os-release 2>/dev/null && echo $PRETTY_NAME"]) or platform.platform(),
        "kernel": platform.release(),
        "cpu": cpu or platform.processor(),
        "logical_cpus": os.cpu_count(),
        "memory": mem,
        "governor": os.environ.get("SHOWCASE_GOVERNOR") or read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        "energy_performance_preference": read("/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference"),
        "turbo": {"0": "on", "1": "off"}.get(read("/sys/devices/system/cpu/intel_pstate/no_turbo") or "", None),
        "loadavg_start": read("/proc/loadavg"),
        "versions": {
            "velt": version([ctx.velt, "--version"]),
            "velt_commit": (commit or "") + (" (with local changes)" if dirty else ""),
            "rustc": version(["rustc", "--version"]),
            "cargo": version(["cargo", "--version"]),
            "clang": version(["clang", "--version"]),
            "node": version([ctx.node, "--version"]) if ctx.node else None,
            "bun": version([ctx.bun, "--version"]) if ctx.bun else None,
            "oha": version([ctx.oha, "--version"]) if ctx.oha else None,
            "python": platform.python_version(),
        },
        "cpu_sets": dict(zip(("server", "load"), servers.cpu_sets())),
    }


PREPARE = {
    "startup": suites.startup_benchmarks,
    "cpu": suites.micro_benchmarks,
    "benchmarks-game": lambda ctx: suites.game_benchmarks(ctx, "benchmarks-game"),
    "regex": lambda ctx: suites.game_benchmarks(ctx, "regex"),
    "async": lambda ctx: suites.async_benchmarks(ctx) + [servers.fetch_benchmarks(ctx)],
    "core": core.core_benchmarks,
    "json": suites.json_benchmarks,
    "tsx": core.tsx_benchmarks,
    "sqlite": suites.sqlite_benchmarks,
    "compile": suites.compile_benchmarks,
    "http": servers.http_benchmarks,
    "websocket": servers.chat_benchmarks,
}


def run_items(ctx, items):
    """items: benchmark dicts (measured by engine), error records, or callables (servers)."""
    records = []
    for item in items:
        if callable(item):
            records += item()
        elif isinstance(item, list):
            records += run_items(ctx, item)
        elif item.get("error") and not item.get("impls"):
            records.append(item)
        else:
            records.append(engine.measure(item, ctx.runs))
    return records


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--smoke", action="store_true")
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--categories", default=",".join(CATEGORIES))
    ap.add_argument("--only", default="")
    ap.add_argument("--velt", default="")
    ap.add_argument("--build-dir", required=True)
    ap.add_argument("--http-duration", default="10s")
    ap.add_argument("--http-conns", type=int, default=256)
    a = ap.parse_args()

    ctx = Ctx()
    ctx.here = os.path.dirname(os.path.abspath(__file__))
    ctx.bench = os.path.dirname(ctx.here)
    ctx.root = os.path.dirname(ctx.bench)
    ctx.out = os.path.abspath(a.build_dir)
    ctx.smoke, ctx.runs = a.smoke, (1 if a.smoke else a.runs)
    ctx.failures, ctx.background = [], []
    ctx.http_duration, ctx.http_conns = a.http_duration, a.http_conns
    ctx.date = datetime.datetime.now().strftime("%Y-%m-%d %H:%M")
    ctx.velt = a.velt
    ctx.node = tool("node")
    ctx.npm = tool("npm")
    ctx.bun = tool("bun", "~/.bun/bin")
    ctx.oha = tool("oha", os.path.join(ctx.out, "..", "tools", "bin"), "~/.cargo/bin")
    only = [s for s in a.only.split(",") if s]
    ctx.wanted_name = lambda name: not only or any(o in name for o in only)
    cats = [c for c in a.categories.split(",") if c]
    unknown = [c for c in cats if c not in PREPARE]
    if unknown:
        sys.exit(f"unknown categories: {', '.join(unknown)} (known: {', '.join(CATEGORIES)})")
    os.makedirs(ctx.out, exist_ok=True)
    os.makedirs(a.out, exist_ok=True)

    info = machine(ctx)
    t0 = time.time()
    # Everything is built (release) before anything is measured.
    prepared = {}
    for c in cats:
        suites.log(f"== preparing {c}")
        items = PREPARE[c](ctx)
        prepared[c] = items if isinstance(items, list) else [items]
    info["build_s"] = round(time.time() - t0, 1)
    results = {"machine": info, "settings": {"runs": ctx.runs, "smoke": ctx.smoke,
                                             "http_duration": ctx.http_duration, "http_conns": ctx.http_conns},
               "build_failures": ctx.failures, "categories": {}}
    for c in cats:
        suites.log(f"== running {c}")
        results["categories"][c] = run_items(ctx, prepared[c])
        # Written after every category, so an interrupted run keeps what it measured.
        info["loadavg_end"] = read("/proc/loadavg")
        info["total_s"] = round(time.time() - t0, 1)
        with open(os.path.join(a.out, "results.json"), "w") as f:
            json.dump(results, f, indent=1)
    report.write(results, a.out)
    suites.log(f"results: {os.path.join(a.out, 'results.json')} and the Markdown tables beside it")


if __name__ == "__main__":
    main()
