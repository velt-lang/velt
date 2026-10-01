#!/usr/bin/env python3
"""Summarize a bench/web results file (OUT.jsonl) as Markdown: one row per test, the best
req/s of each server over the measured levels (its p99 and the server's peak RSS for that test),
and every server's ratio to Rust.

Usage: bench/web/summarize.py results/<run>.jsonl"""
import json
import sys

ORDER = ["velt", "rust", "go", "bun", "node", "node-cluster"]


def label(r):
    if r["test"] in ("queries", "updates"):
        return "%s (N=%d)" % (r["test"], r["queries"])
    return r["test"]


def main(path):
    rows = [json.loads(line) for line in open(path) if line.strip()]
    servers = [s for s in ORDER if any(r["server"] == s for r in rows)]
    best = {}
    tests = []
    for r in rows:
        key = (label(r), r["server"])
        if label(r) not in tests:
            tests.append(label(r))
        if key not in best or r["rps"] > best[key]["rps"]:
            best[key] = r
    print("| test | " + " | ".join(servers) + " |")
    print("|---|" + "---:|" * len(servers))
    for t in tests:
        cells = []
        for s in servers:
            r = best.get((t, s))
            if r is None:
                cells.append("–")
                continue
            ref = best.get((t, "rust"))
            ratio = " (%.2f×)" % (r["rps"] / ref["rps"]) if ref and s != "rust" else ""
            cells.append("%s%s<br>p99 %.1f ms, %.0f MB" % (
                "{:,}".format(r["rps"]), ratio, r["p99_ms"], r["peak_rss_mb"]))
        print("| %s | %s |" % (t, " | ".join(cells)))


if __name__ == "__main__":
    main(sys.argv[1])
