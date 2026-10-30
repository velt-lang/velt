"""The server benchmarks of the showcase: HTTP throughput and latency (bench/http/techempower),
the WebSocket chat app (bench/chat, steady load) and the fetch client (bench/http/fetch).

Servers run pinned to SERVER_CPUS and the load generator to LOAD_CPUS (separate physical cores;
see cpu_sets). Every server gets the same warm-up before it is measured. Only processes started
here are stopped, by the PIDs recorded when they were started.
"""

import json
import os
import signal
import urllib.error
import socket
import subprocess
import sys
import threading
import time
import urllib.request

import core
import engine
from suites import MT, ST, cargo, impl, log, sh, velt_build

ROUTES = ["plaintext", "json", "fortunes"]


def cpu_sets():
    """(server cpus, load cpus) as taskset lists: whole physical cores, half of them (at least
    one) for the load generator, so it can keep up with the fastest servers (each timed run
    records its CPU use, and a run where it is near saturation is flagged). SERVER_CPUS /
    LOAD_CPUS override."""
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
    n_load = max(1, len(groups) // 2)
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


def threads_of(pids):
    """The threads of pids (from /proc/<pid>/status `Threads:`), summed."""
    n = 0
    for p in pids:
        try:
            for line in open(f"/proc/{p}/status"):
                if line.startswith("Threads:"):
                    n += int(line.split()[1])
                    break
        except (OSError, ValueError):
            pass
    return n


class PeakThreads:
    """Samples the server's thread count (all its processes) every `every` seconds while a
    measured run goes on: `with PeakThreads(pids) as t: ...`, then `t.peak`. It shows how many
    threads a server really used, the "1 thread" rows included (the runtime's monitor thread,
    and a spare worker it may start while one is blocked)."""

    def __init__(self, pids, every=0.1):
        # Only the processes that exist now are counted: one forked after sampling starts is not.
        self.pids, self.every, self.peak = [p for root in pids for p in tree(root)], every, 0
        self.done = threading.Event()
        self.thread = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        while True:
            self.peak = max(self.peak, threads_of(self.pids))
            if self.done.wait(self.every):
                return

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *exc):
        self.done.set()
        self.thread.join()
        self.peak = max(self.peak, threads_of(self.pids))


class Server:
    """Server processes started (each in its own process group) and stopped by their PIDs."""

    def __init__(self, cmds, cpus, env=None, probe=None):
        self.procs, self.error = [], None
        try:
            for cmd in cmds:
                self.procs.append(subprocess.Popen(
                    ["taskset", "-c", cpus, *cmd], env={**os.environ, **(env or {})},
                    stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, start_new_session=True))
        except OSError as e:
            self.error = str(e)
        self.pids = [p.pid for p in self.procs]
        self.ok = self.error is None and self._wait(probe)

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
        out = [self.error] if self.error else []
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
    # The Velt server is velt:http/server, written in Velt on std/net (velt/techempower_server.vlt:
    # the routes of techempower/server.vlt, which is on velt:http, hyper in the runtime).
    velt = os.path.join(out, "server-velt")
    verr = velt_build(ctx, os.path.join(ctx.here, "velt", "techempower_server.vlt"), velt)
    axum_t = os.path.join(out, "axum-target")
    aerr = cargo(ctx, "techempower axum", os.path.join(te, "axum"), axum_t)
    n = count_cpus(server_cpus)
    bun = ctx.bun or "bun"
    berr = None if ctx.bun else "bun not found"
    nerr = None if ctx.node else "node not found"
    node = ctx.node or "node"
    bun_ts = os.path.join(ctx.here, "bun", "techempower.ts")
    return [
        # (label, lang, [cmd per process] given the port, env, error, threads: suites.ST or MT)
        ("Velt velt:http/server (written in Velt)", "velt", lambda p: [[velt, str(p)]], {"VELT_THREADS": str(n)}, verr, MT),
        ("Velt velt:http/server (written in Velt, 1 thread)", "velt", lambda p: [[velt, str(p)]], {"VELT_THREADS": "1"}, verr, ST),
        ("Rust axum", "rust", lambda p: [[os.path.join(axum_t, "release", "techempower-axum"), str(p)]], {}, aerr, MT),
        ("Node node:http (1 process)", "node", lambda p: [[node, os.path.join(te, "server.mjs"), str(p)]], {}, nerr,
         ST),
        (f"Node node:cluster ({n} workers)", "node", lambda p: [[node, os.path.join(te, "server.mjs"), str(p)]],
         {"CLUSTER": "1"}, nerr, MT),
        ("Bun Bun.serve (1 process)", "bun", lambda p: [[bun, bun_ts, str(p)]], {}, berr, ST),
        (f"Bun Bun.serve reusePort ({n} processes)", "bun", lambda p: [[bun, bun_ts, str(p)]] * n,
         {"REUSE_PORT": "1"}, berr, MT),
    ]


LOAD_BOUND = 0.9
# oha's own error for a request still in flight when the run's duration ends.
OHA_DEADLINE = "aborted due to deadline"


def seconds(duration):
    """oha's duration ("10s", "500ms", "1m") in seconds."""
    for suffix, scale in (("ms", 0.001), ("s", 1), ("m", 60)):
        if duration.endswith(suffix):
            return float(duration[: -len(suffix)]) * scale
    return float(duration)


def oha(ctx, load_cpus, url, duration, conns):
    """One oha run: req/s, latencies, status codes, and oha's own CPU use (wait4) as a share of
    the load CPUs over the run, so a run where the load generator is the bottleneck shows."""
    cmd = ["taskset", "-c", load_cpus, ctx.oha, "-z", duration, "-c", str(conns), "--no-tui",
           "--output-format", "json", url]
    metrics, out = engine.run_proc(cmd, timeout=600)
    d = json.loads(out)
    s, pct = d["summary"], d.get("latencyPercentiles", {})
    codes = {str(k): v for k, v in d.get("statusCodeDistribution", {}).items()}
    reqs = sum(codes.values())
    util = metrics["cpu_s"] / (count_cpus(load_cpus) * max(seconds(duration), 1e-3))
    return {"rps": s["requestsPerSec"], "success": s["successRate"], "requests": reqs,
            # oha cancels the requests still in flight when the duration ends ("aborted due to
            # deadline"): those are not the server's errors.
            "status_codes": codes,
            "errors": {e: n for e, n in d.get("errorDistribution", {}).items() if e != OHA_DEADLINE},
            "aborted_at_deadline": d.get("errorDistribution", {}).get(OHA_DEADLINE, 0),
            "p50_ms": (pct.get("p50") or 0) * 1000, "p99_ms": (pct.get("p99") or 0) * 1000,
            "avg_ms": (s.get("average") or 0) * 1000,
            "load_cpu_s": metrics["cpu_s"], "load_cpu_share": round(util, 3)}


def fetch(port, route):
    """(status, normalized content-type, body) of one GET."""
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/{route}", timeout=10) as r:
            return r.status, r.headers.get("content-type", "").replace(" ", "").lower(), r.read()
    except urllib.error.HTTPError as e:
        return e.code, (e.headers.get("content-type") or "").replace(" ", "").lower(), e.read()


def check_servers(ctx, servers, server_cpus, routes, errors, reference, expected):
    """Starts each server once and fetches every route: status 200, and the content-type and
    body of the reference server (or of `expected`, {route: body}). A server that fails is an
    error and gets no req/s."""
    got = {}
    for label, lang, cmds, env, err, _ in servers:
        if label in errors:
            continue
        port = free_port()
        log(f"[http] check {label}")
        srv = Server(cmds(port), server_cpus, env, http_probe(port))
        try:
            if not srv.ok:
                errors[label] = f"did not start: {srv.stderr_tail()}"
                continue
            got[label] = {route: fetch(port, route) for route in routes}
        except Exception as e:  # a server that cannot be checked is a failed server
            errors[label] = f"check failed: {type(e).__name__}: {e}"
        finally:
            srv.stop()
    ref = got.get(reference, {})
    for label, responses in got.items():
        for route, (status, ctype, body) in responses.items():
            want = expected.get(route) if expected else (ref.get(route) or (None, None, None))[2]
            want_type = (ref.get(route) or (None, None, None))[1]
            problem = None
            if status != 200:
                problem = f"status {status}"
            elif want is None:
                problem = f"no reference response ({reference} failed)"
            elif body != want:
                problem = f"body differs from the reference ({len(body)} vs {len(want)} bytes)"
            elif want_type is not None and ctype != want_type:
                problem = f"content-type {ctype!r}, the reference sends {want_type!r}"
            if problem and label not in errors:
                errors[label] = f"check failed: /{route}: {problem}"
    return {label: {route: {"status": r[0], "content_type": r[1], "bytes": len(r[2])}
                    for route, r in responses.items()} for label, responses in got.items()}


def http_benchmarks(ctx):
    server_cpus, load_cpus = cpu_sets()
    if not ctx.oha:
        return [{"category": "http", "name": "techempower", "error": "oha not found", "impls": []}]

    servers = http_servers(ctx, server_cpus)
    tsx, tsx_expected = core.tsx_servers(ctx, count_cpus(server_cpus))
    return [lambda: http_run(ctx, servers, server_cpus, load_cpus, ROUTES, "", "Rust axum", None),
            lambda: http_run(ctx, tsx, server_cpus, load_cpus, ["small", "large"], "TSX page ", "Velt TSX",
                             tsx_expected())]


def http_run(ctx, servers, server_cpus, load_cpus, routes, prefix, reference, expected):
    duration = "1s" if ctx.smoke else ctx.http_duration
    warm = "1s" if ctx.smoke else "3s"
    conns = 16 if ctx.smoke else ctx.http_conns
    results = {(label, route): [] for label, *_ in servers for route in routes}
    errors = {label: err for label, _, _, _, err, _ in servers if err}
    checked = check_servers(ctx, servers, server_cpus, routes, errors, reference, expected)
    # A route that answered anything but 200 in a timed run fails for that server only.
    route_errors = {}
    for r in range(ctx.runs):
        k = r % len(servers)
        for label, lang, cmds, env, err, _ in servers[k:] + servers[:k]:
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
                    if (label, route) in route_errors:
                        continue
                    url = f"http://127.0.0.1:{port}/{route}"
                    oha(ctx, load_cpus, url, warm, conns)
                    cpu0, _, _ = proc_stats(srv.pids)
                    with PeakThreads(srv.pids) as threads:
                        m = oha(ctx, load_cpus, url, duration, conns)
                    cpu1, rss, hwm = proc_stats(srv.pids)
                    m["server_peak_threads"] = threads.peak
                    m["server_cpu_s"] = cpu1 - cpu0
                    m["server_us_per_req"] = (cpu1 - cpu0) * 1e6 / max(m["requests"], 1)
                    m["server_rss_mb"], m["server_peak_rss_mb"] = rss, hwm
                    other = {c: n for c, n in m["status_codes"].items() if c != "200"}
                    if other or m["errors"]:
                        route_errors[(label, route)] = (f"round {r + 1}: responses other than 200: "
                                                        f"{other or m['errors']}")
                        continue
                    results[(label, route)].append(m)
            except Exception as e:  # a failed round is a result, never the end of the run
                errors[label] = f"round {r + 1}: {type(e).__name__}: {e}"
            finally:
                srv.stop()
    benches = []
    for route in routes:
        impls = []
        for label, lang, _, _, _, threads in servers:
            runs = results[(label, route)]
            summary = {"runs": len(runs)}
            if runs:
                best = max(runs, key=lambda m: m["rps"])
                summary.update(best_rps=best["rps"], p50_ms=best["p50_ms"], p99_ms=best["p99_ms"],
                               server_us_per_req=min(m["server_us_per_req"] for m in runs),
                               server_peak_rss_mb=max(m["server_peak_rss_mb"] for m in runs),
                               server_peak_threads=max(m.get("server_peak_threads", 0) for m in runs),
                               min_success=min(m["success"] for m in runs),
                               load_cpu_share=best["load_cpu_share"],
                               load_bound=best["load_cpu_share"] >= LOAD_BOUND)
            d = {"label": label, "lang": lang, "threads": threads, "runs": runs, "summary": summary,
                 "response_check": checked.get(label)}
            if label in errors or (label, route) in route_errors:
                d["error"] = errors.get(label) or route_errors[(label, route)]
            impls.append(d)
        benches.append({"category": "http", "name": f"{prefix}/{route}", "kind": "http", "impls": impls,
                        "load": {"tool": "oha", "duration": duration, "warmup": warm, "connections": conns,
                                 "server_cpus": server_cpus, "load_cpus": load_cpus,
                                 "load_bound_share": LOAD_BOUND, "reference": reference}})
    return benches


# ---- WebSocket chat: bench/chat (velt, bun, node), steady load from chat-load ----

def chat_benchmarks(ctx):
    server_cpus, load_cpus = cpu_sets()
    here = os.path.join(ctx.bench, "chat")
    out = os.path.join(ctx.out, "chat")
    os.makedirs(out, exist_ok=True)
    # velt/chat_server.vlt: bench/chat/velt/server.vlt on velt:http/server and velt:websocket's
    # acceptWebSocket (written in Velt) instead of velt:http (hyper in the runtime).
    velt = os.path.join(out, "chat-velt")
    verr = velt_build(ctx, os.path.join(ctx.here, "velt", "chat_server.vlt"), velt)
    load_t = os.path.join(out, "load-target")
    lerr = cargo(ctx, "chat-load (Rust load generator)", os.path.join(here, "load"), load_t)
    nerr = None if ctx.node else "node not found"
    if not nerr and not os.path.isdir(os.path.join(here, "node", "node_modules")):
        nerr = sh(ctx, "npm ci (ws, better-sqlite3)", [ctx.npm or "npm", "ci", "--no-audit", "--no-fund"],
                  cwd=os.path.join(here, "node"))
    n = count_cpus(server_cpus)
    # The Bun and Node chat servers keep the rooms in the process (Bun's pub/sub, ws's client
    # sets): several processes behind reusePort or node:cluster would each see only their own
    # clients, so a room's messages would not reach the members on the other processes. They run
    # as one process, beside Velt on one thread (the like-for-like row) and on the server cores.
    single = "single process; rooms need shared state"
    servers = [
        (f"Velt velt:http/server + velt:websocket (written in Velt, {n} threads)", "velt", [velt],
         {"VELT_THREADS": str(n)}, verr, MT),
        ("Velt velt:http/server + velt:websocket (written in Velt, 1 thread)", "velt", [velt],
         {"VELT_THREADS": "1"}, verr, ST),
        (f"Bun (Bun.serve websockets, {single})", "bun", [ctx.bun or "bun", os.path.join(here, "bun", "server.ts")], {},
         None if ctx.bun else "bun not found", ST),
        (f"Node (ws, {single})", "node", [ctx.node or "node", os.path.join(here, "node", "server.js")], {}, nerr, ST),
    ]
    return lambda: chat_run(ctx, servers, lerr, server_cpus, load_cpus, out, load_t)


def chat_run(ctx, servers, lerr, server_cpus, load_cpus, out, load_t):
    conns, rooms, rate, secs = (100, 10, 100, 2) if ctx.smoke else (2000, 20, 1000, 10)
    loader = os.path.join(load_t, "release", "chat-load")
    rows = {label: [] for label, *_ in servers}
    errors = {label: err or lerr for label, _, _, _, err, _ in servers if err or lerr}
    for r in range(ctx.runs):
        k = r % len(servers)
        for label, lang, cmd, env, _, _ in servers[k:] + servers[:k]:
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
                with PeakThreads(srv.pids) as threads:
                    p = subprocess.run(["taskset", "-c", load_cpus, loader, "steady", f"ws://127.0.0.1:{port}",
                                        str(conns), str(rooms), str(rate), str(secs)],
                                       env={**os.environ, "THREADS": str(count_cpus(load_cpus))},
                                       capture_output=True, text=True, timeout=600)
                cpu1, rss, hwm = proc_stats(srv.pids)
                res = next((json.loads(line) for line in p.stdout.splitlines() if line.startswith("{")), None)
                if res is None:
                    errors[label] = f"round {r + 1}: no result from chat-load: {p.stderr[-300:]}"
                    continue
                res.update(server_cpu_s=cpu1 - cpu0, server_rss_mb=rss, server_peak_rss_mb=hwm,
                           server_peak_threads=threads.peak)
                rows[label].append(res)
            except Exception as e:  # a failed round is a result, never the end of the run
                errors[label] = f"round {r + 1}: {type(e).__name__}: {e}"
            finally:
                srv.stop()
                for suffix in ("", "-wal", "-shm"):
                    if os.path.exists(db + suffix):
                        os.remove(db + suffix)
    impls = []
    for label, lang, cmd, _, _, threads in servers:
        runs = rows[label]
        summary = {"runs": len(runs)}
        if runs:
            best = min(runs, key=lambda m: m["p99_ms"])
            summary.update(p50_ms=best["p50_ms"], p99_ms=best["p99_ms"],
                           deliveries_per_s=best["deliveries_per_s"], delivered_ratio=best["delivered_ratio"],
                           server_cpu_s=min(m["server_cpu_s"] for m in runs),
                           server_peak_rss_mb=max(m["server_peak_rss_mb"] for m in runs),
                           server_peak_threads=max(m.get("server_peak_threads", 0) for m in runs))
        d = {"label": label, "lang": lang, "threads": threads, "runs": runs, "summary": summary}
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
            # reqwest on tokio's multi-thread runtime and Velt's runtime use the server cores;
            # Node and Bun are one process each, paired with Velt on one thread.
            "impls": [impl("Rust reqwest", "rust", [*pin, os.path.join(target, "release", "client"), base, s],
                           threads=MT),
                      impl("Velt fetch", "velt", [*pin, velt, base, s], verr, threads=MT),
                      impl("Velt fetch (1 thread)", "velt", [*pin, velt, base, s], verr, env={"VELT_THREADS": "1"},
                           threads=ST),
                      impl("Node fetch (undici)", "node", [*pin, ctx.node or "node", js, base, s],
                           None if ctx.node else "node not found", threads=ST),
                      impl("Bun fetch", "bun", [*pin, ctx.bun or "bun", js, base, s],
                           None if ctx.bun else "bun not found", threads=ST)],
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
