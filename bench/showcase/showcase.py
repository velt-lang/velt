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


def version(cmd, cwd=None):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=30, cwd=cwd)
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
    rev = subprocess.run(["git", "-C", ctx.root, "rev-parse", "HEAD"], capture_output=True, text=True)
    # An exported tree (git archive) has no .git: VELT_COMMIT names the commit it came from.
    commit = rev.stdout.strip() if rev.returncode == 0 else os.environ.get("VELT_COMMIT", "unknown (not a git checkout)")
    dirty = subprocess.run(["git", "-C", ctx.root, "status", "--porcelain", "--untracked-files=no"],
                           capture_output=True, text=True).stdout.strip()
    return {
        "date": ctx.date,
        # A label, not the host name: results are published.
        "host": os.environ.get("SHOWCASE_HOST_LABEL", "benchmark host"),
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


def failure(category, name, e):
    """The error record of a benchmark (or a whole category) that raised: kept, and the run goes on."""
    suites.log(f"[{category}] {name}: failed: {type(e).__name__}: {e}")
    return {"category": category, "name": name, "error": f"{type(e).__name__}: {e}"[:600], "impls": []}


def run_items(ctx, category, items):
    """items: benchmark dicts (measured by engine), error records, or callables (servers)."""
    records = []
    for item in items:
        try:
            if callable(item):
                records += item()
            elif isinstance(item, list):
                records += run_items(ctx, category, item)
            elif item.get("error") and not item.get("impls"):
                records.append(item)
            else:
                records.append(engine.measure(item, ctx.runs))
        except Exception as e:  # one benchmark's failure is a result, never the end of the run
            name = item.get("name", category) if isinstance(item, dict) else category
            records.append(failure(category, name, e))
    return records


def redact(value, subs):
    """value with machine-specific strings (paths under the repository, the build directory and
    the home directory; the user name) replaced, recursively, so results can be published."""
    if isinstance(value, str):
        for old, new in subs:
            value = value.replace(old, new)
        return value
    if isinstance(value, list):
        return [redact(v, subs) for v in value]
    if isinstance(value, dict):
        return {k: redact(v, subs) for k, v in value.items()}
    return value


def redactions(ctx):
    """The replacements redact makes: longest (most specific) paths first."""
    subs = [(ctx.out, "$BUILD"), (ctx.root, "<repo>"), (os.path.expanduser("~"), "~")]
    user = os.environ.get("USER") or os.environ.get("LOGNAME")
    if user and len(user) > 2:
        subs.append((f"/{user}/", "/<user>/"))
        subs.append((f"/{user}", "/<user>"))
    return [(a, b) for a, b in subs if a and a != "/"]


def package_versions(ctx):
    """The installed npm packages the JS rows use, and each SQLite driver's SQLite version."""
    out = {}
    for d, pkgs in ((os.path.join(ctx.here, "tsx"), ("react", "react-dom", "preact", "preact-render-to-string")),
                    (os.path.join(ctx.bench, "db", "node"), ("better-sqlite3",)),
                    (os.path.join(ctx.bench, "chat", "node"), ("ws", "better-sqlite3"))):
        for pkg in pkgs:
            try:
                with open(os.path.join(d, "node_modules", pkg, "package.json")) as f:
                    out[f"npm {pkg} ({os.path.relpath(d, ctx.root)})"] = json.load(f)["version"]
            except (OSError, ValueError, KeyError):
                pass
    q = "select sqlite_version() as v"
    if ctx.node:
        out["sqlite (better-sqlite3)"] = version(
            [ctx.node, "-e", f"console.log(new (require('better-sqlite3'))(':memory:').prepare('{q}').get().v)"],
            cwd=os.path.join(ctx.bench, "db", "node"))
    if ctx.bun:
        out["sqlite (bun:sqlite)"] = version(
            [ctx.bun, "-e", "const { Database } = require('bun:sqlite'); "
                            f"console.log(new Database(':memory:').query('{q}').get().v)"])
    exe = os.path.join(ctx.out, "sqlite", "sqlite-version")
    os.makedirs(os.path.dirname(exe), exist_ok=True)
    if suites.velt_build(ctx, os.path.join(ctx.here, "programs", "sqlite_version.vlt"), exe) is None:
        out["sqlite (velt:sqlite)"] = version([exe])
    for lock in (os.path.join(ctx.bench, "db", "rust", "Cargo.lock"),
                 os.path.join(ctx.bench, "sqlite", "rust", "Cargo.lock")):
        lines = (read(lock) or "").splitlines()
        for k, line in enumerate(lines[:-1]):
            if line == 'name = "libsqlite3-sys"':
                # The crate's version, not SQLite's: each libsqlite3-sys release bundles one SQLite.
                out[f"libsqlite3-sys crate (rusqlite, bundled SQLite; {os.path.relpath(lock, ctx.root)})"] = \
                    lines[k + 1].split('"')[1]
    return out


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
        try:
            items = PREPARE[c](ctx)
            prepared[c] = items if isinstance(items, list) else [items]
        except Exception as e:  # a category that cannot be built is reported, the others still run
            prepared[c] = [failure(c, f"{c} (build)", e)]
    try:
        info["versions"].update(package_versions(ctx))
    except Exception as e:
        info["versions"]["packages"] = f"not recorded: {e}"
    info["build_s"] = round(time.time() - t0, 1)
    results = {"machine": info, "settings": {"runs": ctx.runs, "smoke": ctx.smoke,
                                             "http_duration": ctx.http_duration, "http_conns": ctx.http_conns},
               "build_failures": ctx.failures, "categories": {}}
    subs = redactions(ctx)

    def save():
        info["loadavg_end"] = read("/proc/loadavg")
        info["total_s"] = round(time.time() - t0, 1)
        with open(os.path.join(a.out, "results.json"), "w") as f:
            json.dump(redact(results, subs), f, indent=1)

    try:
        for c in cats:
            suites.log(f"== running {c}")
            results["categories"][c] = run_items(ctx, c, prepared[c])
            # Written after every category, so an interrupted run keeps what it measured.
            save()
    finally:
        save()
        report.write(redact(results, subs), a.out)
        suites.log(f"results: {os.path.join(a.out, 'results.json')} and the Markdown tables beside it")


if __name__ == "__main__":
    main()
