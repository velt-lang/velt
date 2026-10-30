#!/usr/bin/env python3
"""Markdown tables from results.json: one file per category, a memory table and an index
(README.md) with the machine, the versions and every benchmark where Velt is not the fastest."""

import json
import os
import sys

TITLES = {
    "startup": "Start-up time and binary size",
    "cpu": "CPU micro-benchmarks",
    "benchmarks-game": "The Benchmarks Game",
    "regex": "Regular expressions",
    "async": "Async and concurrency",
    "json": "JSON",
    "sqlite": "SQLite",
    "compile": "Compile time",
    "http": "HTTP servers",
    "websocket": "WebSocket chat server",
    "core": "Core features",
    "tsx": "TSX server-side rendering",
    "memory": "Memory (peak RSS)",
}
HIGHER_IS_BETTER = {"ops/s", "Mops/s"}  # the other units (s, ms) are times


def num(x, digits=3):
    if x is None:
        return ""
    if abs(x) >= 1000:
        return f"{x:,.0f}"
    if abs(x) >= 100:
        return f"{x:.0f}"
    if abs(x) >= 10:
        return f"{x:.1f}"
    return f"{x:.{digits}f}"


def versus(velt, other, higher_better):
    """How Velt compares with this implementation, from the two values."""
    if not velt or not other:
        return ""
    r = (velt / other) if higher_better else (other / velt)
    if abs(r - 1) < 0.005:
        return "same"
    return f"Velt {r:.2f}× faster" if r > 1 else f"Velt {1 / r:.2f}× slower"


def velt_ref(impls, key):
    for i in impls:
        if i["lang"] == "velt" and i.get("summary", {}).get("runs"):
            v = key(i)
            if v:
                return v
    return None


def process_table(rec, lines, notes, ms=False):
    impls = rec["impls"]
    best = lambda i: i.get("summary", {}).get("best_wall_s")
    vref = velt_ref(impls, best)
    unit = "ms" if ms else "s"
    scale = 1000 if ms else 1
    extra_mb = rec.get("input_bytes") and rec.get("rounds")
    for i in impls:
        s = i.get("summary", {})
        if not i.get("error") and rec.get("runs_factor") == 0:
            lines.append(f"| {rec['name']} | {i['label']} | output checked, not timed | | | | | |")
            continue
        if not usable(i):
            lines.append(f"| {rec['name']} | {i['label']} | n/a | | | | | |")
            notes.append(f"{rec['name']}, {i['label']}: {i.get('error', 'no runs')}")
            continue
        if i.get("error"):
            notes.append(f"{rec['name']}, {i['label']}: partial, {i['error']}")
        mbs = ""
        if extra_mb:
            mbs = (f" ({rec['input_bytes'] * rec['rounds'] / s['best_wall_s'] / 1e6:,.0f} MB/s, "
                   f"{s['best_wall_s'] * 1000 / rec['rounds']:.2f} ms/op)")
        lines.append(f"| {rec['name']} | {i['label']}{' (partial)' if i.get('error') else ''} | {num(s['best_wall_s'] * scale)}{mbs} | "
                     f"{num(s['median_wall_s'] * scale)} | {num(s['min_cpu_s'])} | {num(s['peak_rss_mb'], 1)} | "
                     f"{s['runs']} | {versus(vref, s['best_wall_s'], False)} |")
    return f"| benchmark | implementation | best wall {unit} | median wall {unit} | CPU s (user+sys) | peak RSS MB | runs | vs Velt |\n|---|---|---:|---:|---:|---:|---:|---|"


def reported_table(rec, lines, notes):
    impls, unit = rec["impls"], rec.get("unit", "s")
    hb = unit in HIGHER_IS_BETTER
    keys = []
    for i in impls:
        for k in i.get("summary", {}).get("reported", {}):
            if k not in keys:
                keys.append(k)
    for i in impls:
        if not usable(i):
            lines.append(f"| {rec['name']} | {i['label']} | n/a | | |")
            notes.append(f"{rec['name']}, {i['label']}: {i.get('error', 'no runs')}")
        elif i.get("error"):
            notes.append(f"{rec['name']}, {i['label']}: partial, {i['error']}")
    for k in keys:
        vref = velt_ref(impls, lambda i: i.get("summary", {}).get("reported", {}).get(k))
        for i in impls:
            v = i.get("summary", {}).get("reported", {}).get(k)
            if v is None or not usable(i):
                continue
            lines.append(f"| {rec['name']}: {k} | {i['label']}{' (partial)' if i.get('error') else ''} | {num(v)} | "
                         f"{num(i['summary']['peak_rss_mb'], 1)} | {versus(vref, v, hb)} |")
    label = {"s": "client's own time, s", "ms": "the program's own time, ms", "ops/s": "ops/s (best)",
             "Mops/s": "Mops/s (best)"}[unit]
    return f"| benchmark | implementation | {label} | peak RSS MB | vs Velt |\n|---|---|---:|---:|---|"


def http_table(recs, lines, notes):
    for rec in recs:
        impls = rec["impls"]
        vref = velt_ref(impls, lambda i: i["summary"].get("best_rps"))
        for i in impls:
            s = i["summary"]
            if not s.get("runs"):
                lines.append(f"| {rec['name']} | {i['label']} | n/a | | | | | | |")
                notes.append(f"{rec['name']}, {i['label']}: {i.get('error', 'no runs')}")
                continue
            ok = "" if s["min_success"] >= 1 else f" ({s['min_success'] * 100:.1f}% ok)"
            bound = " **load-bound**" if s.get("load_bound") else ""
            label = f"{i['label']}{' (partial)' if i.get('error') else ''}"
            lines.append(f"| {rec['name']} | {label} | {s['best_rps']:,.0f}{ok}{bound} | {s['p50_ms']:.2f} | "
                         f"{s['p99_ms']:.2f} | {s['server_us_per_req']:.1f} | {s['server_peak_rss_mb']:.1f} | "
                         f"{s.get('load_cpu_share', 0) * 100:.0f}% | {versus(vref, s['best_rps'], True)} |")
            if i.get("error"):
                notes.append(f"{rec['name']}, {i['label']}: {i['error']}")
    load = recs[0]["load"] if recs else {}
    head = ("| route | server | req/s (best) | p50 ms | p99 ms | server CPU µs/request | server peak RSS MB "
            "| load generator CPU | vs Velt |\n"
            "|---|---|---:|---:|---:|---:|---:|---:|---|")
    note = (f"\nLoad: {load.get('tool')} for {load.get('duration')} per run with {load.get('connections')} "
            f"keep-alive connections after a {load.get('warmup')} warm-up (the same for every server); "
            f"servers on CPUs {load.get('server_cpus')}, the load generator on CPUs {load.get('load_cpus')}. "
            "Latencies are from the run with the best req/s. Every server's responses were checked "
            f"first (status 200, and the body and content-type of {load.get('reference')}'s, or the "
            "rendered page's). *Load generator CPU* is oha's CPU time over the run as a share of its "
            f"CPUs; at {load.get('load_bound_share', 0.9) * 100:.0f}% or more the row is **load-bound**: "
            "the load generator, not the server, may have set the req/s.\n") if load else ""
    return head, note


def ws_table(recs, lines, notes):
    for rec in recs:
        impls = rec["impls"]
        vref = velt_ref(impls, lambda i: i["summary"].get("p99_ms"))
        for i in impls:
            s = i["summary"]
            if not s.get("runs"):
                lines.append(f"| {rec['name']} | {i['label']} | n/a | | | | | | |")
                notes.append(f"{rec['name']}, {i['label']}: {i.get('error', 'no runs')}")
                continue
            lines.append(f"| {rec['name']} | {i['label']} | {s['p50_ms']:.2f} | {s['p99_ms']:.2f} | "
                         f"{s['deliveries_per_s']:,.0f} | {s['delivered_ratio']:.3f} | {s['server_cpu_s']:.2f} | "
                         f"{s['server_peak_rss_mb']:.1f} | {versus(vref, s['p99_ms'], False)} (p99) |")
    return ("| scenario | server | p50 ms | p99 ms | deliveries/s | delivered | server CPU s | server peak RSS MB | vs Velt |\n"
            "|---|---|---:|---:|---:|---:|---:|---:|---|")


def category_md(cat, recs):
    out = [f"# {TITLES.get(cat, cat)}", ""]
    notes, lines = [], []
    if cat == "http":
        head, note = http_table(recs, lines, notes)
        out += [head, *lines, note]
    elif cat == "websocket":
        out += [ws_table(recs, lines, notes), *lines, ""]
    else:
        groups = {}
        for rec in recs:
            if rec.get("error") and not rec.get("impls"):
                notes.append(f"{rec['name']}: {rec['error']}")
                continue
            kind = "reported" if rec.get("parse") else "process"
            g = groups.setdefault(kind, ([], None))
            if kind == "reported":
                head = reported_table(rec, g[0], notes)
            else:
                head = process_table(rec, g[0], notes, ms=(cat == "startup"))
            groups[kind] = (g[0], head)
        for kind, (rows, head) in groups.items():
            out += [head, *rows, ""]
        if cat == "startup":
            for rec in recs:
                if rec.get("sizes"):
                    out += ["| file | size |", "|---|---:|"]
                    for k, v in rec["sizes"].items():
                        what = "executable" if k in ("Rust", "Velt") else "runtime executable (the program needs it installed)"
                        out.append(f"| {k} {what} | {v / 1e6:.2f} MB |")
                    out.append("")
    if cat == "cpu":
        out += ["`hashmap`: each language's default hash map with its default hash function (Rust: "
                "`HashMap` with SipHash; Velt, Node and Bun: `Map`).", ""]
    if notes:
        out += ["Not measured (n/a):", ""] + [f"- {n}" for n in notes] + [""]
    return "\n".join(out)


def headline(impls, lang):
    for i in impls:
        if i["lang"] == lang:
            return i
    return None


def memory_md(results):
    out = [f"# {TITLES['memory']}", "",
           "Peak RSS (MB) of the first listed implementation per language in each benchmark (Velt "
           "`main`, the first Rust program by file name, Node and Bun `main.js`, and so on); every "
           "implementation's peak RSS is in its category's table. Servers: peak RSS of the server "
           "process(es).", "",
           "| category | benchmark | Velt | Rust | Node | Bun |", "|---|---|---:|---:|---:|---:|"]
    for cat, recs in results["categories"].items():
        if cat == "compile":
            continue
        for rec in recs:
            cells = []
            for lang in ("velt", "rust", "node", "bun"):
                i = headline(rec.get("impls", []), lang)
                s = (i or {}).get("summary", {})
                v = s.get("peak_rss_mb", s.get("server_peak_rss_mb"))
                cells.append(num(v, 1) if v is not None and not (i or {}).get("error") else "")
            if any(cells):
                out.append(f"| {cat} | {rec['name']} | " + " | ".join(cells) + " |")
    return "\n".join(out) + "\n"


def usable(i):
    """An implementation with timed runs: compared, even when a later round failed (partial)."""
    return bool(i.get("summary", {}).get("runs"))


def partial(i):
    return f" (partial: {i['error']})" if i.get("error") and usable(i) else ""


def metrics(cat, rec):
    """[(metric name, value of an implementation, higher is better)] for a record."""
    if cat == "http" or rec.get("kind") == "http":
        return [("req/s", lambda i: i["summary"].get("best_rps"), True)]
    if cat == "websocket" or rec.get("kind") == "websocket":
        return [("p99", lambda i: i["summary"].get("p99_ms"), False)]
    if rec.get("parse"):
        keys = []
        for i in rec.get("impls", []):
            for k in i.get("summary", {}).get("reported", {}):
                if k not in keys:
                    keys.append(k)
        hb = rec.get("unit") in HIGHER_IS_BETTER
        return [(k, (lambda i, k=k: i["summary"].get("reported", {}).get(k)), hb) for k in keys]
    return [("", lambda i: i["summary"].get("best_wall_s"), False)]


def losses(results):
    """Every benchmark where Velt is not the fastest: another implementation that ran (partial
    runs included) beat Velt's first listed implementation with results, or no Velt
    implementation produced a result at all. Compile times are not ranked."""
    out = []
    for cat, recs in results["categories"].items():
        if cat == "compile":
            continue
        for rec in recs:
            if rec.get("runs_factor") == 0:
                continue
            impls = rec.get("impls", [])
            veltish = [i for i in impls if i["lang"] == "velt" and "n/a:" not in i.get("error", "")[:4]]
            if rec.get("error") and not impls:
                out.append(f"{cat}: {rec['name']}: Velt: n/a (the benchmark failed: {rec['error']})")
                continue
            if not veltish:
                continue
            for metric, key, hb in metrics(cat, rec):
                label = f"{rec['name']}: {metric}" if metric else rec["name"]
                v = next((i for i in veltish if usable(i) and key(i)), None)
                if v is None:
                    reason = veltish[0].get("error") or "no result"
                    out.append(f"{cat}: {label}: Velt: n/a ({reason})")
                    continue
                others = [(key(i), i) for i in impls if i["lang"] != "velt" and usable(i) and key(i)]
                if not others:
                    continue
                bv, bi = (max if hb else min)(others, key=lambda o: o[0])
                if (bv > key(v)) if hb else (bv < key(v)):
                    # Either row at the load generator's limit: the gap may be oha's, not the servers'.
                    bound = " (load-bound)" if bi["summary"].get("load_bound") or v["summary"].get("load_bound") else ""
                    out.append(f"{cat}: {label}: {bi['label']}{partial(bi)} ({versus(key(v), bv, hb)}){bound}")
    return out


def write(results, out_dir):
    m = results["machine"]
    idx = ["# Benchmark showcase results", "",
           f"- Date: {m['date']}; host {m['host']}: {m['cpu']}, {m['logical_cpus']} logical CPUs, {m['memory']}",
           f"- OS: {m['os']}, kernel {m['kernel']}; governor {m['governor']}, energy preference "
           f"{m.get('energy_performance_preference')}, turbo {m.get('turbo')}",
           f"- Load average at start: {m['loadavg_start']}; at end: {m.get('loadavg_end')}",
           f"- Settings: {json.dumps(results['settings'])}; build {m.get('build_s')} s, total {m.get('total_s')} s",
           "- Versions: " + "; ".join(f"{k} {v}" for k, v in m["versions"].items() if v),
           f"- CPU sets for servers: server {m['cpu_sets']['server']}, load generator {m['cpu_sets']['load']}",
           "", "## Categories", ""]
    for cat, recs in results["categories"].items():
        with open(os.path.join(out_dir, f"{cat}.md"), "w") as f:
            f.write(category_md(cat, recs))
        idx.append(f"- [{TITLES.get(cat, cat)}]({cat}.md)")
    with open(os.path.join(out_dir, "memory.md"), "w") as f:
        f.write(memory_md(results))
    idx.append(f"- [{TITLES['memory']}](memory.md)")
    lost = losses(results)
    idx += ["", "## Where Velt is not the fastest", "",
            "Every benchmark where another implementation beat Velt's first listed one (compile "
            "times are listed in their own table and not ranked):", ""]
    idx += [f"- {x}" for x in lost] or ["- none"]
    if results.get("build_failures"):
        idx += ["", "## Build failures", ""] + [f"- {x}" for x in results["build_failures"]]
    with open(os.path.join(out_dir, "README.md"), "w") as f:
        f.write("\n".join(idx) + "\n")


if __name__ == "__main__":
    # Re-render the tables of an existing run: report.py <results dir>
    d = sys.argv[1]
    write(json.load(open(os.path.join(d, "results.json"))), d)
