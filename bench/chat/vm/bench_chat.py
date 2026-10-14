#!/usr/bin/env python3
"""chatbench on a many-core Linux VM (../SPEC.md): each server pinned to the same physical cores,
the load generator on the others (taskset); CPU, context switches and memory from /proc.

  bench_chat.py [suite ...]   suites: join steady ramp percore topics idle (default: all)

Server cores: one vCPU per physical core, cores 0..CORES-1 (vCPUs 0,2,..); load generator on
vCPUs 16-47. One JSON line per run goes to ~/chat-results.jsonl and stdout, after a line with
the versions of the servers' toolchains (suite "versions").
"""

import json, os, subprocess, sys, time

HOME = os.path.expanduser("~")
C = f"{HOME}/velt/bench/chat"
LOAD = f"{C}/load/target/release/chat-load"
BUN = f"{HOME}/.bun/bin/bun"
CLIENT_CPUS = "16-47"
CLIENT_THREADS = "16"
CORES = int(os.environ.get("CORES", "8"))
OUT = open(os.environ.get("CHAT_OUT", f"{HOME}/chat-results.jsonl"), "a")
# CHAT_DBDIR=/dev/shm/... keeps the disk out of the measurement (cloud disks throttle by burst
# credits, so fsync stalls vary from run to run).
DBDIR = os.environ.get("CHAT_DBDIR", f"{HOME}/chatdb")
os.makedirs(DBDIR, exist_ok=True)


def server_cpus(n):
    return ",".join(str(2 * i) for i in range(n))


def server(name, cores):
    if name == "velt":
        return [f"{C}/.work/chat-velt"], {"VELT_THREADS": str(cores)}
    if name == "velt-topics":
        return [f"{C}/.work/chat-velt-topics"], {"VELT_THREADS": str(cores)}
    if name == "velt-per-core":
        return [f"{C}/.work/chat-velt"], {"VELT_THREADS": str(cores), "VELT_SCHEDULER": "per-core"}
    if name == "go":
        return [f"{C}/go/chat-go"], {"GOMAXPROCS": str(cores)}
    if name == "bun":
        return [BUN, f"{C}/bun/server.ts"], {}
    if name == "node":
        return ["node", f"{C}/node/server.js"], {}
    raise SystemExit(f"unknown server {name}")


def proc(pid):
    with open(f"/proc/{pid}/stat") as f:
        parts = f.read().rsplit(")", 1)[1].split()
    cpu = (int(parts[11]) + int(parts[12])) / os.sysconf("SC_CLK_TCK")
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


PORT = [5100]


def run(suite, name, cores, load_args, label=None):
    PORT[0] += 1
    port = PORT[0]
    cmd, env = server(name, cores)
    db = f"{DBDIR}/{name}-{port}.db"
    p = subprocess.Popen(["taskset", "-c", server_cpus(cores)] + cmd + [str(port)],
                         env={**os.environ, **env, "DB": db}, stdout=subprocess.DEVNULL,
                         stderr=subprocess.PIPE)
    for _ in range(200):
        if subprocess.run(["bash", "-c", f"exec 3<>/dev/tcp/127.0.0.1/{port}"], capture_output=True).returncode == 0:
            break
        time.sleep(0.05)
    time.sleep(0.5)
    cpu0, cs0, idle_rss, _ = proc(p.pid)
    lcmd = ["taskset", "-c", CLIENT_CPUS, LOAD, load_args[0], f"ws://127.0.0.1:{port}"] + [str(a) for a in load_args[1:]]
    t0 = time.time()
    lp = subprocess.Popen(lcmd, env={**os.environ, "THREADS": CLIENT_THREADS}, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, text=True)
    res, ready = None, None
    for line in lp.stdout:
        if line.startswith("READY"):
            time.sleep(1.0)
            ready = proc(p.pid)
        elif line.startswith("{"):
            res = json.loads(line)
    lp.wait()
    wall = time.time() - t0
    alive = p.poll() is None
    cpu1, cs1, rss1, hwm = proc(p.pid) if alive else (cpu0, cs0, 0, 0)
    p.terminate()
    try:
        err = p.communicate(timeout=10)[1].decode(errors="replace")[-300:]
    except subprocess.TimeoutExpired:
        p.kill()
        err = "killed"
    for suffix in ["", "-wal", "-shm"]:
        try:
            os.remove(db + suffix)
        except FileNotFoundError:
            pass
    row = {"suite": suite, "server": label or name, "cores": cores, "wall_s": round(wall, 2),
           "idle_rss_mb": round(idle_rss, 1), "peak_rss_mb": round(hwm, 1), "crashed": not alive,
           **(res or {"error": "no result"})}
    if ready is not None:
        # CPU and memory from READY (every client connected) to the end of the run.
        row["cpu_s"] = round(cpu1 - ready[0], 2)
        row["ctx_switches"] = cs1 - ready[1]
        row["ready_rss_mb"] = round(ready[2], 1)
        if res and res.get("joined", res.get("conns")):
            n = res.get("joined", res.get("conns"))
            row["kb_per_conn"] = round((ready[2] - idle_rss) * 1024 / n, 2)
    else:
        row["cpu_s"] = round(cpu1 - cpu0, 2)
        row["ctx_switches"] = cs1 - cs0
    if err.strip() and err != "killed":
        row["stderr"] = err.strip()
    print(json.dumps(row), flush=True)
    OUT.write(json.dumps(row) + "\n")
    OUT.flush()
    time.sleep(1)
    return row


SERVERS = ["velt", "velt-per-core", "go", "bun", "node"]


def suite_join():
    for s in SERVERS:
        run("join", s, CORES, ["join", 20000, 200])


def suite_steady():
    for s in SERVERS:
        run("steady", s, CORES, ["steady", 10000, 100, 1000, 15])


def ramp(s, cores, suite):
    rate = 1000
    while rate <= 256000:
        r = run(suite, s, cores, ["steady", 10000, 100, rate, 10])
        if r.get("crashed") or r.get("p99_ms", 1e9) > 100 or r.get("delivered_ratio", 0) < 0.95:
            break
        rate *= 2


def suite_ramp():
    for s in SERVERS:
        ramp(s, CORES, "ramp")


def suite_percore():
    # Velt and Go held to one core, against the single-threaded Bun and Node (ramp above).
    for s in ["velt", "go"]:
        ramp(s, 1, "ramp-1core")


def suite_topics():
    # Rooms as std/websocket topics (velt/server-topics.vlt) against the plain send loop.
    for rate in [4000, 8000, 16000]:
        for s in ["velt", "velt-topics"]:
            run("topics", s, CORES, ["steady", 10000, 100, rate, 10])


def suite_idle():
    for s in SERVERS:
        run("idle", s, CORES, ["idle", 50000, 500, 5])


SUITES = {"join": suite_join, "steady": suite_steady, "ramp": suite_ramp, "percore": suite_percore,
          "topics": suite_topics, "idle": suite_idle}


def versions():
    def out(cmd):
        try:
            return subprocess.run(cmd, capture_output=True, text=True).stdout.strip() or None
        except OSError:
            return None
    try:
        velt = open(f"{HOME}/velt-commit").read().strip()  # written by up.sh
    except OSError:
        velt = out(["git", "-C", C, "rev-parse", "HEAD"])
    return {"suite": "versions", "velt": velt,
            "go": out(["/usr/local/go/bin/go", "version"]), "bun": out([BUN, "--version"]),
            "node": out(["node", "--version"]), "kernel": os.uname().release}


if __name__ == "__main__":
    row = versions()
    print(json.dumps(row), flush=True)
    OUT.write(json.dumps(row) + "\n")
    OUT.flush()
    for name in sys.argv[1:] or list(SUITES):
        SUITES[name]()
