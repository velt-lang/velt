#!/usr/bin/env python3
"""WebSocket benchmark on a many-core Linux VM: server and load generator pinned to separate
physical cores (taskset), CPU time / context switches / memory read from /proc.

  bench_vm.py [suite ...]   suites: scale pipe bcast fixed conns (default: all)

Server cores: one vCPU per physical core, cores 0..7 (vCPUs 0,2,..14; the SMT siblings stay
idle). Load generator: physical cores 8..23 with their siblings (vCPUs 16-47).
One JSON line per run goes to results.jsonl (appended) and stdout.
"""

import json, os, subprocess, sys, time

HOME = os.path.expanduser("~")
B = f"{HOME}/velt/bench/websocket"
W = f"{B}/.work"
LOAD = f"{B}/load/target/release/ws-load"
RUST = f"{B}/rust/target/release/ws-bench-rust"
BUN = f"{HOME}/.bun/bin/bun"
CLIENT_CPUS = "16-47"
CLIENT_THREADS = "16"
SECS = float(os.environ.get("SECS", "8"))
OUT = open(f"{HOME}/results.jsonl", "a")


def server_cpus(n):
    return ",".join(str(2 * i) for i in range(n))


# name -> (command, env) ; `t` is the worker-thread count where it applies.
def server(name, t):
    if name == "velt-raw-steal":
        return [f"{W}/velt-raw-server"], {"VELT_SCHEDULER": "stealing", "VELT_THREADS": str(t)}
    if name == "velt-raw-per-core":
        return [f"{W}/velt-raw-server"], {"VELT_SCHEDULER": "per-core", "VELT_THREADS": str(t)}
    if name == "velt":
        return [f"{W}/velt-server"], {"VELT_THREADS": str(t)}
    if name == "rust-raw":
        return [RUST], {"MODE": "raw", "READ_BUF": "8192", "TOKIO_WORKER_THREADS": str(t)}
    if name == "rust-shard":
        return [RUST], {"MODE": "shard", "READ_BUF": "8192", "SHARDS": str(t)}
    if name == "bun":
        return [BUN, f"{B}/bun/server.ts"], {}
    if name == "node":
        return ["node", f"{B}/node/server.mjs"], {}
    raise SystemExit(f"unknown server {name}")


def proc_cpu(pid):
    """(cpu seconds, context switches, rss MB, peak rss MB) of pid and all its threads."""
    with open(f"/proc/{pid}/stat") as f:
        parts = f.read().rsplit(")", 1)[1].split()
    tick = os.sysconf("SC_CLK_TCK")
    cpu = (int(parts[11]) + int(parts[12])) / tick
    cs = 0
    for tid in os.listdir(f"/proc/{pid}/task"):
        try:
            for line in open(f"/proc/{pid}/task/{tid}/status"):
                if "ctxt_switches" in line:
                    cs += int(line.split()[1])
        except FileNotFoundError:
            pass
    rss = hwm = 0
    for line in open(f"/proc/{pid}/status"):
        if line.startswith("VmRSS:"):
            rss = int(line.split()[1]) / 1024
        if line.startswith("VmHWM:"):
            hwm = int(line.split()[1]) / 1024
    return cpu, cs, rss, hwm


PORT = [5000]


def run(suite, name, t, cores, load_args, extra_env=None, ready_rss=False):
    PORT[0] += 1
    port = PORT[0]
    cmd, env = server(name, t)
    p = subprocess.Popen(["taskset", "-c", server_cpus(cores)] + cmd + [str(port)],
                         env={**os.environ, **env}, stdout=subprocess.DEVNULL,
                         stderr=subprocess.PIPE)
    for _ in range(100):
        r = subprocess.run(["bash", "-c", f"exec 3<>/dev/tcp/127.0.0.1/{port}"],
                           capture_output=True)
        if r.returncode == 0:
            break
        time.sleep(0.05)
    time.sleep(0.3)
    cpu0, cs0, idle_rss, _ = proc_cpu(p.pid)
    lenv = {**os.environ, "THREADS": CLIENT_THREADS, **(extra_env or {})}
    mode = load_args[0]
    path = "/bcast" if mode == "bcast" else "/echo"
    lcmd = ["taskset", "-c", CLIENT_CPUS, LOAD, mode, f"ws://127.0.0.1:{port}{path}"] + [str(a) for a in load_args[1:]]
    t0 = time.time()
    lp = subprocess.Popen(lcmd, env=lenv, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    res, rrss = None, None
    for line in lp.stdout:
        if line.startswith("READY"):
            time.sleep(1.5)
            rrss = proc_cpu(p.pid)[2]
        elif line.startswith("{"):
            res = json.loads(line)
    lp.wait()
    wall = time.time() - t0
    alive = p.poll() is None
    cpu1, cs1, _, hwm = proc_cpu(p.pid) if alive else (cpu0, cs0, 0, 0)
    p.terminate()
    try:
        err = p.communicate(timeout=10)[1].decode(errors="replace")[-200:]
    except subprocess.TimeoutExpired:
        p.kill()
        err = "killed"
    row = {"suite": suite, "server": name, "threads": t, "cores": cores,
           "cpu_s": round(cpu1 - cpu0, 2), "wall_s": round(wall, 2),
           "ctx_switches": cs1 - cs0, "idle_rss_mb": round(idle_rss, 1),
           "peak_rss_mb": round(hwm, 1), "crashed": not alive, **(res or {"error": "no result"})}
    if rrss is not None:
        row["ready_rss_mb"] = round(rrss, 1)
        if res and res.get("opened"):
            row["kb_per_conn"] = round((rrss - idle_rss) * 1024 / res["opened"], 2)
    msgs = (res or {}).get("msgs") or 0
    if msgs and row["cpu_s"] > 0:
        row["us_per_msg"] = round(row["cpu_s"] * 1e6 / msgs, 3)
        row["cs_per_msg"] = round(row["ctx_switches"] / msgs, 4)
    if err.strip() and err != "killed":
        row["stderr"] = err.strip()
    line = json.dumps(row)
    print(line, flush=True)
    OUT.write(line + "\n")
    OUT.flush()
    time.sleep(0.5)


SCALED = ["velt-raw-steal", "velt-raw-per-core", "rust-raw", "rust-shard"]
SINGLE = ["bun", "node"]
THREADS = [1, 2, 4, 8]


def suite_scale():
    # One message in flight per connection: scheduler cost per message.
    for t in THREADS:
        for s in SCALED + ["velt"]:
            run("scale", s, t, t, ["echo", 1000, SECS])
    for s in SINGLE:
        run("scale", s, 1, 1, ["echo", 1000, SECS])


def suite_pipe():
    # 64 messages in flight per connection: the server is the limit.
    for t in THREADS:
        for s in SCALED + ["velt"]:
            run("pipe", s, t, t, ["echo", 400, SECS], {"INFLIGHT": "64"})
    for s in SINGLE:
        run("pipe", s, 1, 1, ["echo", 400, SECS], {"INFLIGHT": "64"})


def suite_bcast():
    for t in THREADS:
        for s in ["velt-raw-steal", "velt-raw-per-core", "velt"]:
            run("bcast", s, t, t, ["bcast", 1000, 8, SECS])
    for s in SINGLE + ["rust-raw"]:
        run("bcast", s, 1, 1, ["bcast", 1000, 8, SECS])


def suite_fixed():
    # The same offered load for everyone: server CPU per message.
    for rate in [100000, 400000]:
        for t in [1, 2, 4, 8]:
            for s in ["velt-raw-steal", "velt-raw-per-core", "rust-raw"]:
                run(f"fixed-{rate}", s, t, t, ["rate", 1000, rate, SECS])
        for s in SINGLE:
            run(f"fixed-{rate}", s, 1, 1, ["rate", 1000, rate, SECS])


def suite_conns():
    servers = [("velt", 4), ("velt-raw-steal", 4), ("velt-raw-per-core", 4), ("rust-raw", 4),
               ("bun", 1), ("node", 1)]
    for s, t in servers:
        run("conns", s, t, max(t, 1), ["conns", 20000, 3], ready_rss=True)


SUITES = {"scale": suite_scale, "pipe": suite_pipe, "bcast": suite_bcast,
          "fixed": suite_fixed, "conns": suite_conns}

if __name__ == "__main__":
    for name in sys.argv[1:] or list(SUITES):
        SUITES[name]()
