"""The server benchmarks of the showcase: HTTP throughput and latency (bench/http/techempower),
the WebSocket chat app (bench/chat, steady load) and the fetch client (bench/http/fetch).

Servers run pinned to SERVER_CPUS and the load generator to LOAD_CPUS (separate physical cores;
see cpu_sets). Every server gets the same warm-up before it is measured. Only processes started
here are stopped, by the PIDs recorded when they were started.
"""

import json
import os
import signal
import socket
import subprocess
import sys
import time
import urllib.request

import core
import engine
from suites import cargo, impl, log, sh, velt_build

ROUTES = ["plaintext", "json", "fortunes"]


def cpu_sets():
    """(server cpus, load cpus) as taskset lists: whole physical cores, about a third of them
    (at least one) for the load generator. SERVER_CPUS / LOAD_CPUS override."""
    if os.environ.get("SERVER_CPUS") and os.environ.get("LOAD_CPUS"):
        return os.environ["SERVER_CPUS"], os.environ["LOAD_CPUS"]
    cores = {}
    for cpu in sorted(os.sched_getaffinity(0)):
        try:
            sib = open(f"/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list").read().strip()
        except OSError:
            sib = str(cpu)
        cores.setdefault(sib, []).append(cpu)
    groups = list(cores.values())
    n_load = max(1, len(groups) // 3)
    if len(groups) < 2:
        cpus = ",".join(str(c) for c in groups[0])
        return cpus, cpus
    server = sorted(c for g in groups[:-n_load] for c in g)
    load = sorted(c for g in groups[-n_load:] for c in g)
    return ",".join(map(str, server)), ",".join(map(str, load))


def count_cpus(spec):
    n = 0
    for part in spec.split(","):
        a, _, b = part.partition("-")
        n += int(b) - int(a) + 1 if b else 1
    return n


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def tree(pid):
    """pid and all its descendants (from /proc)."""
    parents = {}
    for d in os.listdir("/proc"):
        if d.isdigit():
            try:
                stat = open(f"/proc/{d}/stat").read()
                parents.setdefault(int(stat.rsplit(")", 1)[1].split()[1]), []).append(int(d))
            except (OSError, IndexError, ValueError):
                pass
    out, todo = [], [pid]
    while todo:
        p = todo.pop()
        out.append(p)
        todo += parents.get(p, [])
    return out


def proc_stats(pids):
    """(cpu seconds, RSS MB, peak RSS MB) summed over pids and their descendants."""
    cpu = rss = hwm = 0.0
    tick = os.sysconf("SC_CLK_TCK")
    for root in pids:
        for p in tree(root):
            try:
                parts = open(f"/proc/{p}/stat").read().rsplit(")", 1)[1].split()
                cpu += (int(parts[11]) + int(parts[12])) / tick
                for line in open(f"/proc/{p}/status"):
                    if line.startswith("VmRSS:"):
                        rss += int(line.split()[1]) / 1024
                    elif line.startswith("VmHWM:"):
                        hwm += int(line.split()[1]) / 1024
            except (OSError, IndexError, ValueError):
                pass
    return cpu, rss, hwm


class Server:
    """Server processes started (each in its own process group) and stopped by their PIDs."""

    def __init__(self, cmds, cpus, env=None, probe=None):
        self.procs = []
        for cmd in cmds:
            self.procs.append(subprocess.Popen(
                ["taskset", "-c", cpus, *cmd], env={**os.environ, **(env or {})},
                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, start_new_session=True))
        self.pids = [p.pid for p in self.procs]
        self.ok = self._wait(probe)

    def _wait(self, probe):
        for _ in range(300):
            if any(p.poll() is not None for p in self.procs):
                return False
            try:
                probe()
                return True
            except OSError:
                time.sleep(0.05)
        return False

    def stderr_tail(self):
        out = []
        for p in self.procs:
            if p.poll() is not None and p.stderr:
                out += p.stderr.read().decode(errors="replace").strip().splitlines()[-3:]
        return " | ".join(out)

    def stop(self):
        for p in self.procs:
            if p.poll() is None:
                os.killpg(p.pid, signal.SIGTERM)
        for p in self.procs:
            try:
                p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
                p.wait()


def http_probe(port):
    def probe():
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/plaintext", timeout=1) as r:
            r.read()
    return probe


def tcp_probe(port):
    def probe():
        socket.create_connection(("127.0.0.1", port), timeout=1).close()
    return probe


# ---- HTTP: bench/http/techempower plus a Bun.serve counterpart ----

def http_servers(ctx, server_cpus):
    te = os.path.join(ctx.bench, "http", "techempower")
    out = os.path.join(ctx.out, "http")
    os.makedirs(out, exist_ok=True)
    velt = os.path.join(out, "server-velt")
    verr = velt_build(ctx, os.path.join(te, "server.vlt"), velt)
    axum_t = os.path.join(out, "axum-target")
    aerr = cargo(ctx, "techempower axum", os.path.join(te, "axum"), axum_t)
    n = count_cpus(server_cpus)
    bun = ctx.bun or "bun"
    berr = None if ctx.bun else "bun not found"
    nerr = None if ctx.node else "node not found"
    node = ctx.node or "node"
    bun_ts = os.path.join(ctx.here, "bun", "techempower.ts")
    return [
        # (label, lang, [cmd per process] given the port, env, error)
        ("Velt velt:http", "velt", lambda p: [[velt, str(p)]], {}, verr),
        ("Rust axum", "rust", lambda p: [[os.path.join(axum_t, "release", "techempower-axum"), str(p)]], {}, aerr),
        ("Node node:http (1 process)", "node", lambda p: [[node, os.path.join(te, "server.mjs"), str(p)]], {}, nerr),
        (f"Node node:cluster ({n} workers)", "node", lambda p: [[node, os.path.join(te, "server.mjs"), str(p)]],
         {"CLUSTER": "1"}, nerr),
        ("Bun Bun.serve (1 process)", "bun", lambda p: [[bun, bun_ts, str(p)]], {}, berr),
        (f"Bun Bun.serve reusePort ({n} processes)", "bun", lambda p: [[bun, bun_ts, str(p)]] * n,
         {"REUSE_PORT": "1"}, berr),
    ]


def oha(ctx, load_cpus, url, duration, conns):
    cmd = ["taskset", "-c", load_cpus, ctx.oha, "-z", duration, "-c", str(conns), "--no-tui",
           "--output-format", "json", url]
    p = subprocess.run(cmd, capture_output=True, timeout=600)
    if p.returncode != 0:
        raise engine.RunError(p.stderr.decode(errors="replace")[-300:])
    d = json.loads(p.stdout)
    s, pct = d["summary"], d.get("latencyPercentiles", {})
    reqs = sum(d.get("statusCodeDistribution", {}).values())
    return {"rps": s["requestsPerSec"], "success": s["successRate"], "requests": reqs,
            "p50_ms": pct.get("p50", 0) * 1000, "p99_ms": pct.get("p99", 0) * 1000,
            "avg_ms": s["average"] * 1000}


def http_benchmarks(ctx):
    server_cpus, load_cpus = cpu_sets()
    if not ctx.oha:
        return [{"category": "http", "name": "techempower", "error": "oha not found", "impls": []}]

    servers = http_servers(ctx, server_cpus)
    tsx = core.tsx_servers(ctx)
    return lambda: (http_run(ctx, servers, server_cpus, load_cpus, ROUTES, "")
                    + http_run(ctx, tsx, server_cpus, load_cpus, ["small", "large"], "TSX page "))


def http_run(ctx, servers, server_cpus, load_cpus, routes, prefix):
    duration = "1s" if ctx.smoke else ctx.http_duration
    warm = "1s" if ctx.smoke else "3s"
    conns = 16 if ctx.smoke else ctx.http_conns
    results = {(label, route): [] for label, *_ in servers for route in routes}
    errors = {label: err for label, _, _, _, err in servers if err}
    for r in range(ctx.runs):
        k = r % len(servers)
        for label, lang, cmds, env, err in servers[k:] + servers[:k]:
            if label in errors:
                continue
            port = free_port()
            log(f"[http] round {r + 1}/{ctx.runs} {label}")
            srv = Server(cmds(port), server_cpus, env, http_probe(port))
            try:
                if not srv.ok:
                    errors[label] = f"did not start: {srv.stderr_tail()}"
                    continue
                for route in routes:
                    url = f"http://127.0.0.1:{port}/{route}"
                    oha(ctx, load_cpus, url, warm, conns)
                    cpu0, _, _ = proc_stats(srv.pids)
                    m = oha(ctx, load_cpus, url, duration, conns)
                    cpu1, rss, hwm = proc_stats(srv.pids)
                    m["server_cpu_s"] = cpu1 - cpu0
                    m["server_us_per_req"] = (cpu1 - cpu0) * 1e6 / max(m["requests"], 1)
                    m["server_rss_mb"], m["server_peak_rss_mb"] = rss, hwm
                    results[(label, route)].append(m)
            except (engine.RunError, ValueError, KeyError, subprocess.TimeoutExpired) as e:
                errors[label] = f"round {r + 1}: {e}"
            finally:
                srv.stop()
    benches = []
    for route in routes:
        impls = []
        for label, lang, _, _, _ in servers:
            runs = results[(label, route)]
            summary = {"runs": len(runs)}
            if runs:
                best = max(runs, key=lambda m: m["rps"])
                summary.update(best_rps=best["rps"], p50_ms=best["p50_ms"], p99_ms=best["p99_ms"],
                               server_us_per_req=min(m["server_us_per_req"] for m in runs),
                               server_peak_rss_mb=max(m["server_peak_rss_mb"] for m in runs),
                               min_success=min(m["success"] for m in runs))
            d = {"label": label, "lang": lang, "runs": runs, "summary": summary}
            if label in errors:
                d["error"] = errors[label]
            impls.append(d)
        benches.append({"category": "http", "name": f"{prefix}/{route}", "kind": "http", "impls": impls,
                        "load": {"tool": "oha", "duration": duration, "warmup": warm, "connections": conns,
                                 "server_cpus": server_cpus, "load_cpus": load_cpus}})
    return benches


# ---- WebSocket chat: bench/chat (velt, bun, node), steady load from chat-load ----

def chat_benchmarks(ctx):
    server_cpus, load_cpus = cpu_sets()
    here = os.path.join(ctx.bench, "chat")
    out = os.path.join(ctx.out, "chat")
    os.makedirs(out, exist_ok=True)
    velt = os.path.join(out, "chat-velt")
    verr = velt_build(ctx, os.path.join(here, "velt", "server.vlt"), velt)
    load_t = os.path.join(out, "load-target")
    lerr = cargo(ctx, "chat-load (Rust load generator)", os.path.join(here, "load"), load_t)
    nerr = None if ctx.node else "node not found"
    if not nerr and not os.path.isdir(os.path.join(here, "node", "node_modules")):
        nerr = sh(ctx, "npm ci (ws, better-sqlite3)", [ctx.npm or "npm", "ci", "--no-audit", "--no-fund"],
                  cwd=os.path.join(here, "node"))
    n = count_cpus(server_cpus)
    servers = [
        (f"Velt ({n} threads)", "velt", [velt], {"VELT_THREADS": str(n)}, verr),
        ("Bun (Bun.serve websockets, 1 process)", "bun", [ctx.bun or "bun", os.path.join(here, "bun", "server.ts")], {},
         None if ctx.bun else "bun not found"),
        ("Node (ws, 1 process)", "node", [ctx.node or "node", os.path.join(here, "node", "server.js")], {}, nerr),
    ]
    return lambda: chat_run(ctx, servers, lerr, server_cpus, load_cpus, out, load_t)


def chat_run(ctx, servers, lerr, server_cpus, load_cpus, out, load_t):
    conns, rooms, rate, secs = (100, 10, 100, 2) if ctx.smoke else (2000, 20, 1000, 10)
    loader = os.path.join(load_t, "release", "chat-load")
    rows = {label: [] for label, *_ in servers}
    errors = {label: err or lerr for label, _, _, _, err in servers if err or lerr}
    for r in range(ctx.runs):
        k = r % len(servers)
        for label, lang, cmd, env, _ in servers[k:] + servers[:k]:
            if label in errors:
                continue
            port = free_port()
            db = os.path.join(out, f"chat-{port}.db")
            log(f"[websocket] round {r + 1}/{ctx.runs} {label}")
            srv = Server([cmd + [str(port)]], server_cpus, {**env, "DB": db}, tcp_probe(port))
            try:
                if not srv.ok:
                    errors[label] = f"did not start: {srv.stderr_tail()}"
                    continue
                time.sleep(0.5)
                cpu0, _, _ = proc_stats(srv.pids)
                p = subprocess.run(["taskset", "-c", load_cpus, loader, "steady", f"ws://127.0.0.1:{port}",
                                    str(conns), str(rooms), str(rate), str(secs)],
                                   env={**os.environ, "THREADS": str(count_cpus(load_cpus))},
                                   capture_output=True, text=True, timeout=600)
                cpu1, rss, hwm = proc_stats(srv.pids)
                res = next((json.loads(line) for line in p.stdout.splitlines() if line.startswith("{")), None)
                if res is None:
                    errors[label] = f"round {r + 1}: no result from chat-load: {p.stderr[-300:]}"
                    continue
                res.update(server_cpu_s=cpu1 - cpu0, server_rss_mb=rss, server_peak_rss_mb=hwm)
                rows[label].append(res)
            finally:
                srv.stop()
                for suffix in ("", "-wal", "-shm"):
                    if os.path.exists(db + suffix):
                        os.remove(db + suffix)
    impls = []
    for label, lang, cmd, _, _ in servers:
        runs = rows[label]
        summary = {"runs": len(runs)}
        if runs:
            best = min(runs, key=lambda m: m["p99_ms"])
            summary.update(p50_ms=best["p50_ms"], p99_ms=best["p99_ms"],
                           deliveries_per_s=best["deliveries_per_s"], delivered_ratio=best["delivered_ratio"],
                           server_cpu_s=min(m["server_cpu_s"] for m in runs),
                           server_peak_rss_mb=max(m["server_peak_rss_mb"] for m in runs))
        d = {"label": label, "lang": lang, "runs": runs, "summary": summary}
        if label in errors:
            d["error"] = errors[label]
        impls.append(d)
    return [{"category": "websocket", "name": f"chat steady: {conns} clients, {rooms} rooms, {rate} msg/s, {secs} s",
             "kind": "websocket", "impls": impls,
             "load": {"tool": "bench/chat/load (chat-load steady)", "server_cpus": server_cpus, "load_cpus": load_cpus}}]


# ---- fetch client: bench/http/fetch (Velt fetch, Node and Bun fetch, Rust reqwest) ----

def fetch_benchmarks(ctx):
    server_cpus, load_cpus = cpu_sets()
    here = os.path.join(ctx.bench, "http", "fetch")
    out = os.path.join(ctx.out, "fetch")
    os.makedirs(out, exist_ok=True)
    target = os.path.join(out, "rust-target")
    rerr = cargo(ctx, "fetch: hyper server and reqwest client", os.path.join(here, "rust"), target)
    velt = os.path.join(out, "client-velt")
    verr = velt_build(ctx, os.path.join(here, "client.vlt"), velt)
    if rerr:
        return [{"category": "async", "name": "fetch client", "error": rerr, "impls": []}]

    port = free_port()
    base = f"http://127.0.0.1:{port}"
    pin = ["taskset", "-c", server_cpus]
    benches = []
    for s in ("seq", "conc", "big", "json", "gzip"):
        js = os.path.join(here, "client.mjs")
        benches.append({
            "category": "async", "name": f"fetch client: {s}", "check": "fetch", "parse": "fetch", "unit": "s",
            "impls": [impl("Rust reqwest", "rust", [*pin, os.path.join(target, "release", "client"), base, s]),
                      impl("Velt fetch", "velt", [*pin, velt, base, s], verr),
                      impl("Node fetch (undici)", "node", [*pin, ctx.node or "node", js, base, s],
                           None if ctx.node else "node not found"),
                      impl("Bun fetch", "bun", [*pin, ctx.bun or "bun", js, base, s],
                           None if ctx.bun else "bun not found")],
        })

    def run():
        # The hyper server on the load cores; the clients (what is measured) on the server cores.
        srv = Server([[os.path.join(target, "release", "server"), str(port)]], load_cpus, None, tcp_probe(port))
        try:
            if not srv.ok:
                for b in benches:
                    for i in b["impls"]:
                        i["error"] = f"fetch server did not start: {srv.stderr_tail()}"
            return [engine.measure(b, ctx.runs) for b in benches]
        finally:
            srv.stop()
    return run
