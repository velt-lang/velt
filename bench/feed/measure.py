#!/usr/bin/env python3
"""Server CPU per request for implementations of the feed API (bench/feed/README.md).

  measure.py --seed <dir with feed.db and tokens.json> [--secs 8] [--rounds 3] [--conns 64]
             [--tests feed,post,like,create,mix] name=/path/to/server ...

Each server runs with one worker (VELT_THREADS=1, TOKIO_WORKER_THREADS=1) on a fresh copy of
the seed database per test, loaded by wrk. The number that matters on a one-core box is the
CPU the server burns per request (user + system, from ps), so that is what is reported, with
req/s and peak RSS next to it. Rounds are interleaved across servers; the best round counts.
"""
import argparse, json, os, re, shutil, signal, subprocess, sys, tempfile, time, urllib.request

LUA = r"""
local tokens = { %s }
local test = "%s"
local n = 0
math.randomseed(os.time())
local function auth() return { ["Authorization"] = "Bearer " .. tokens[math.random(#tokens)], ["Content-Type"] = "application/json" } end
local function feed() return wrk.format("GET", "/feed") end
local function post() return wrk.format("GET", "/posts/" .. math.random(500000)) end
local function like() return wrk.format("POST", "/posts/" .. math.random(500000) .. "/like", auth()) end
local function create()
  n = n + 1
  return wrk.format("POST", "/posts", auth(), '{"body":"someone says hi at 2026-01-01T00:00:00.000Z (VU 1, iter ' .. n .. ')"}')
end
-- The load test's mix: per loop one feed, one post, 0.15 likes, 0.02 creates.
local function mix()
  local r = math.random() * 2.17
  if r < 1 then return feed() elseif r < 2 then return post() elseif r < 2.15 then return like() else return create() end
end
local pick = { feed = feed, post = post, like = like, create = create, mix = mix }
request = function() return pick[test]() end
"""


def cpu_seconds(pid):
    out = subprocess.run(["ps", "-o", "utime=,stime=", "-p", str(pid)], capture_output=True, text=True).stdout
    total = 0.0
    for field in out.split():
        parts = [float(p) for p in field.split(":")]
        secs = 0.0
        for p in parts:
            secs = secs * 60 + p
        total += secs
    return total


def rss_mb(pid):
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout
    return int(out.strip() or 0) / 1024


def healthy(port):
    try:
        return urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=1).status == 200
    except Exception:
        return False


def wrk(port, script, secs, conns):
    out = subprocess.run(
        ["wrk", "-t2", f"-c{conns}", f"-d{secs}s", "--latency", "-s", script, f"http://127.0.0.1:{port}"],
        capture_output=True, text=True).stdout
    reqs = int(re.search(r"(\d+) requests in", out).group(1))
    bad = re.search(r"Non-2xx or 3xx responses: (\d+)", out)
    p99 = re.search(r"99%\s+(\S+)", out).group(1)
    return reqs, int(bad.group(1)) if bad else 0, p99


def run_one(binary, test, args, tokens, port):
    work = tempfile.mkdtemp(prefix="feed-bench-")
    db = os.path.join(work, "feed.db")
    shutil.copy(os.path.join(args.seed, "feed.db"), db)
    script = os.path.join(work, "load.lua")
    with open(script, "w") as f:
        f.write(LUA % (",".join(f'"{t}"' for t in tokens), test))
    env = dict(os.environ, SQLITE_PATH=db, JWT_SECRET="twelve-dollar-challenge", HOST="127.0.0.1",
               PORT=str(port), VELT_THREADS="1", TOKIO_WORKER_THREADS="1")
    proc = subprocess.Popen([binary], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(100):
            if healthy(port):
                break
            time.sleep(0.1)
        else:
            raise RuntimeError(f"{binary} did not come up")
        wrk(port, script, 1, args.conns)  # warm-up
        before = cpu_seconds(proc.pid)
        reqs, bad, p99 = wrk(port, script, args.secs, args.conns)
        cpu = cpu_seconds(proc.pid) - before
        return {"us": cpu / reqs * 1e6, "rps": reqs / args.secs, "bad": bad, "p99": p99, "rss": rss_mb(proc.pid)}
    finally:
        proc.send_signal(signal.SIGKILL)
        proc.wait()
        shutil.rmtree(work, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", required=True)
    ap.add_argument("--secs", type=int, default=8)
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--conns", type=int, default=64)
    ap.add_argument("--tests", default="feed,post,like,create,mix")
    ap.add_argument("servers", nargs="+")
    args = ap.parse_args()
    servers = [s.split("=", 1) for s in args.servers]
    with open(os.path.join(args.seed, "tokens.json")) as f:
        tokens = [t["token"] for t in json.load(f)[:200]]
    best = {}
    port = 3200
    for test in args.tests.split(","):
        for _ in range(args.rounds):
            for name, binary in servers:
                port += 1
                r = run_one(binary, test, args, tokens, port)
                key = (test, name)
                if key not in best or r["us"] < best[key]["us"]:
                    best[key] = r
    first = servers[0][0]
    print("| test | server | CPU µs/req | × %s | req/s | p99 | RSS MB | non-2xx |" % first)
    print("|---|---|---:|---:|---:|---:|---:|---:|")
    for test in args.tests.split(","):
        for name, _ in servers:
            r = best[(test, name)]
            ratio = r["us"] / best[(test, first)]["us"]
            print(f"| {test} | {name} | {r['us']:.1f} | {ratio:.2f} | {r['rps']:,.0f} | {r['p99']} | {r['rss']:.0f} | {r['bad']} |")


if __name__ == "__main__":
    sys.exit(main())
