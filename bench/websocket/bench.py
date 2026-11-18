#!/usr/bin/env python3
"""Runs the WebSocket benchmark (README.md) against each server; standard library only.

  bench.py [server ...]     servers: see SERVERS (default: velt node bun rust-raw rust-hyper);
                            env QUICK=1 for short runs, SCENARIOS=a,b to pick scenarios

Every scenario starts the server fresh, samples its RSS and CPU time every 100 ms while
ws-load drives it, and appends one JSON line per scenario to results/<timestamp>.jsonl.
"""

import json
import os
import socket
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
LOAD = os.path.join(HERE, "load/target/release/ws-load")
WORK = os.environ.get("WORK", os.path.join(HERE, ".work"))
QUICK = os.environ.get("QUICK") == "1"
SECS = 3 if QUICK else 10
PORT = 9300
MAX_CONNS = int(os.environ.get("MAX_CONNS", "15000"))

RUST = os.path.join(HERE, "rust/target/release/ws-bench-rust")
# name: (command, extra environment). The default run is velt node bun rust-raw rust-hyper; the
# others are for digging into Velt's numbers (README.md "Investigating").
SERVERS = {
    "velt": ([os.path.join(WORK, "velt-server")], {}),
    "node": (["node", os.path.join(HERE, "node/server.mjs")], {}),
    "bun": (["bun", os.path.join(HERE, "bun/server.ts")], {}),
    "rust-raw": ([RUST], {"MODE": "raw"}),
    "rust-hyper": ([RUST], {"MODE": "hyper"}),
    "rust-raw-8k": ([RUST], {"MODE": "raw", "READ_BUF": "8192"}),
    "rust-hyper-8k": ([RUST], {"MODE": "hyper", "READ_BUF": "8192"}),
    "rust-hyper-1t": ([RUST], {"MODE": "hyper", "TOKIO_WORKER_THREADS": "1"}),
    "velt-raw": ([os.path.join(WORK, "velt-raw-server")], {}),
    "velt-raw-1t": ([os.path.join(WORK, "velt-raw-server")], {"VELT_THREADS": "1"}),
    "velt-1t": ([os.path.join(WORK, "velt-server")], {"VELT_THREADS": "1"}),
}
DEFAULT_SERVERS = ["velt", "node", "bun", "rust-raw", "rust-hyper"]

SCENARIOS = [
    ("echo-1", ["echo", "/echo", "1", SECS]),
    ("echo-100", ["echo", "/echo", "100", SECS]),
    ("echo-1000", ["echo", "/echo", "1000", SECS]),
    ("echo-100-4KiB", ["echo", "/echo", "100", SECS, "4096"]),
    ("bcast-1000x8", ["bcast", "/bcast", "1000", "8", SECS]),
    ("bcast-5000x2", ["bcast", "/bcast", "5000", "2", SECS]),
    ("conns", ["conns", "/echo", str(MAX_CONNS), "5"]),
]


def ps(pid):
    """(rss MB, cpu seconds) of pid, or None once it is gone."""
    out = subprocess.run(["ps", "-o", "rss=,time=", "-p", str(pid)], capture_output=True, text=True).stdout.split()
    if len(out) < 2:
        return None
    t = 0.0
    for part in out[1].split(":"):
        t = t * 60 + float(part)
    return int(out[0]) / 1024, t


class Sampler(threading.Thread):
    def __init__(self, pid):
        super().__init__(daemon=True)
        self.pid, self.peak, self.stop, self.last = pid, 0.0, False, None

    def run(self):
        while not self.stop:
            s = ps(self.pid)
            if s:
                self.peak = max(self.peak, s[0])
                self.last = s
            time.sleep(0.1)


def wait_port(port):
    for _ in range(100):
        try:
            socket.create_connection(("127.0.0.1", port), 0.2).close()
            return
        except OSError:
            time.sleep(0.1)
    raise SystemExit(f"server did not listen on {port}")


def run(server, name, args, out):
    global PORT
    PORT += 1
    if name == "conns":
        time.sleep(35)  # TIME_WAIT on the client's ephemeral ports (macOS: 2 × 15 s)
    cmd, env = SERVERS[server]
    proc = subprocess.Popen(cmd + [str(PORT)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env={**os.environ, **env})
    wait_port(PORT)
    time.sleep(0.5)
    idle_rss, cpu0 = ps(proc.pid)
    sampler = Sampler(proc.pid)
    sampler.start()
    cmd = [LOAD, args[0], f"ws://127.0.0.1:{PORT}{args[1]}"] + [str(a) for a in args[2:]]
    t0 = time.time()
    load = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    ready_rss = None
    result = None
    for line in load.stdout:
        if line.startswith("READY"):
            time.sleep(1.0)  # let the server settle with every connection open
            ready_rss = ps(proc.pid)[0]
        elif line.startswith("{"):
            result = json.loads(line)
    load.wait()
    wall = time.time() - t0
    sampler.stop = True
    sampler.join()
    end = ps(proc.pid)
    proc.terminate()
    err = proc.communicate(timeout=10)[1].decode(errors="replace").strip()
    row = {
        "server": server,
        "scenario": name,
        "idle_rss_mb": round(idle_rss, 1),
        "peak_rss_mb": round(sampler.peak, 1),
        "cpu_s": round((end or sampler.last)[1] - cpu0, 2),
        "wall_s": round(wall, 2),
        "crashed": end is None,
        **(result or {"error": "no result"}),
    }
    if ready_rss is not None:
        row["ready_rss_mb"] = round(ready_rss, 1)
        if result and result.get("opened"):
            row["kb_per_conn"] = round((ready_rss - idle_rss) * 1024 / result["opened"], 1)
    if err:
        row["stderr"] = err[-300:]
    print(json.dumps(row), flush=True)
    out.write(json.dumps(row) + "\n")
    out.flush()


def main():
    servers = sys.argv[1:] or DEFAULT_SERVERS
    os.makedirs(os.path.join(HERE, "results"), exist_ok=True)
    path = os.path.join(HERE, "results", time.strftime("%Y%m%d-%H%M%S") + ".jsonl")
    only = os.environ.get("SCENARIOS")
    with open(path, "w") as out:
        for name, args in SCENARIOS:
            if only and name not in only.split(","):
                continue
            for server in servers:
                run(server, name, args, out)
    print(f"results: {path}", file=sys.stderr)


if __name__ == "__main__":
    main()
