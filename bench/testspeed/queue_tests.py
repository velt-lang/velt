#!/usr/bin/env python3
"""Merge queue `check (ubuntu-latest)` jobs side by side: what made one slow?

For each merge queue run in the window prints which clang objects it restored and how old they
were, whether the robustness sweep ran in full, the gate steps' seconds, and three sums of test
seconds (nextest's per-test times, in a 4-thread job about 4 times the step's wall time):

  sweep    the robustness tests and the formatter's corpus, which VELT_ROBUSTNESS picks
  clang    tests that build programs with --release, which the clang objects can serve
  front    tests of sema, the parser and the loader, which never start clang: the runner's speed

Compare `clang` and `front` between runs: a runner twice as slow doubles both, a stale clang cache
only `clang`.

  python3 bench/testspeed/queue_tests.py --since 2026-11-06T20:00 [--limit 30] [--logs DIR]
"""
import argparse
import json
import os
import re
import statistics
import subprocess
from datetime import datetime

REPO = "velt-lang/velt"
ANSI = re.compile(r"\x1b\[[0-9;]*m")
PASS = re.compile(r"PASS\s+\[\s*([\d.]+)s\]\s+(?:\(\s*\d+/\d+\)\s+)?(\S+)\s+(\S+)")
STEP = re.compile(r"==> (.*)")
RESTORED = re.compile(r"Cache hit for restore-key: velt-clang-objects-\S+-(\d+)")
SWEEP = ("velt_syntax::robustness", "velt_fmt::corpus")
FRONT = ("velt_sema", "velt_syntax", "velt_loader")
SECONDS_OF = ("unit + integration tests", "VIR identity (std up front, std on demand)")


def gh(*args):
    out = subprocess.run(["gh", *args], capture_output=True, check=True).stdout
    return out.decode("utf8", "replace")


def api(path):
    return json.loads(gh("api", path))


def stamp(line):
    return datetime.fromisoformat(line[:26])


def log_of(job, cache):
    path = os.path.join(cache, f"{job}.log")
    if not os.path.exists(path):
        text = gh("api", "--allow-escape-sequences", f"repos/{REPO}/actions/jobs/{job}/logs")
        with open(path, "w", encoding="utf8") as f:
            f.write(text)
    with open(path, encoding="utf8", errors="replace") as f:
        return f.read()


def read(text, clang_binaries):
    row = {"mode": "", "from": "", "steps": {}, "sweep": 0.0, "clang": 0.0, "front": 0.0}
    last = None
    for raw in text.splitlines():
        line = ANSI.sub("", raw)
        if not row["mode"] and "VELT_ROBUSTNESS:" in line:
            row["mode"] = line.split("VELT_ROBUSTNESS:")[1].strip() or "default"
        m = RESTORED.search(line)
        if m:
            row["from"] = m.group(1)
        m = STEP.search(line)
        if m:
            when = stamp(line)
            if last:
                row["steps"][last[0]] = (when - last[1]).total_seconds()
            last = (m.group(1), when)
        m = PASS.search(line)
        if m:
            seconds, binary = float(m.group(1)), m.group(2)
            if binary in SWEEP:
                row["sweep"] += seconds
            elif binary.split("::")[0] in FRONT:
                row["front"] += seconds
            elif binary in clang_binaries:
                row["clang"] += seconds
    return row


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--since", required=True)
    p.add_argument("--limit", type=int, default=30)
    p.add_argument("--logs", default=".queue-logs")
    p.add_argument("--clang", default="veltc::packages,veltc::std_tests,veltc::example_apps")
    args = p.parse_args()
    os.makedirs(args.logs, exist_ok=True)
    clang = set(args.clang.split(","))
    runs = api(
        f"repos/{REPO}/actions/workflows/ci.yml/runs?event=merge_group&per_page=100"
        f"&created=%3E{args.since}"
    )["workflow_runs"]
    rows = []
    for run in runs[: args.limit]:
        jobs = api(f"repos/{REPO}/actions/runs/{run['id']}/jobs?per_page=100")["jobs"]
        job = next((j for j in jobs if j["name"] == "check (ubuntu-latest)"), None)
        if not job or job["conclusion"] != "success":
            continue
        row = read(log_of(job["id"], args.logs), clang)
        row["run"], row["at"] = run["id"], run["created_at"][:16]
        rows.append(row)
    print("started           run          objects from  sweep   tests   VIR  |  sweep  clang  front")
    for r in sorted(rows, key=lambda r: r["at"]):
        tests, vir = (round(r["steps"].get(k, 0)) for k in SECONDS_OF)
        print(
            f"{r['at']}  {r['run']}  {r['from']:>12}  {r['mode']:8}{tests:5} {vir:5}  | "
            f"{r['sweep']:6.0f} {r['clang']:6.0f} {r['front']:6.0f}"
        )
    if rows:
        print("median clang/front:", round(statistics.median(r["clang"] / r["front"] for r in rows), 2))


if __name__ == "__main__":
    main()
