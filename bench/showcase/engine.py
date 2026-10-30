"""Runs one benchmark's implementations interleaved and checks that they agree.

A benchmark is a dict:
  name, category, unit ("s" for whole-process wall time, or what `parse` reports),
  impls: [{label, lang, cmd: [...], stdin: path|None, env: {...}|None}],
  check: "ref" (byte-equal to the first implementation's output), "file:<path>" (byte-equal to
         that file), "json" (parses to the same value as the first implementation's output),
         "results" / "cmap" / "fetch" (the parsed checksums equal the first implementation's),
         or "none",
  parse: None (whole-process metrics only), "results" (`RESULT <workload> <ops> <checksum> <ms>`
         lines: ops/s per workload), "cmap" (`<name> <threads> <mops> <check>` lines) or "fetch"
         (`<scenario>: ... <seconds>s` lines: the client's own time).

Every implementation first runs once untimed: that run is the correctness check, and it warms the
page cache for every implementation alike. Then `runs` rounds follow, each running every
implementation once, the order rotated by one per round, so a change of load on the machine hits
them all alike. Wall time, user + sys CPU time and peak RSS come from the measure helper
(measure.c): its own fork, exec and wait4 of the program, the numbers GNU `time -v` prints,
without needing it installed. Not Python's wait4: see measure.c for why that reports the
harness's own RSS.
"""

import json
import os
import re
import subprocess
import sys
import tempfile
import threading
import time

TIMEOUT_S = 900


class RunError(Exception):
    pass


# The measure helper (measure.c, built by build_measure): None until it is built and checked.
MEASURE = None


def build_measure(out_dir, cc=None):
    """Builds measure.c into out_dir; returns its path, or raises RunError with the reason."""
    src = os.path.join(os.path.dirname(os.path.abspath(__file__)), "measure.c")
    exe = os.path.join(out_dir, "measure")
    reason = "no C compiler found (cc, clang or gcc)"
    for compiler in [c for c in (cc, os.environ.get("CC"), "cc", "clang", "gcc") if c]:
        try:
            p = subprocess.run([compiler, "-O2", "-o", exe, src], capture_output=True, text=True)
        except OSError:
            continue
        if p.returncode == 0:
            return exe
        reason = p.stderr.strip()[-300:]
    raise RunError(f"could not build measure.c: {reason}")


def check_measure():
    """None when the helper reports each process's own peak RSS, else the reason: a program that
    touches 128 MB must report over 120 MB, and `true` under 16 MB even though this process
    (whose RSS the old wait4 measurement reported for every small program) now holds 64 MB more."""
    ballast = b"x" * (64 << 20)
    try:
        small = run_proc(["true"])[0]["rss_mb"]
        big = run_proc([sys.executable, "-c", "b = b'x' * (128 << 20)"])[0]["rss_mb"]
    except RunError as e:
        return f"the helper failed: {e}"
    finally:
        del ballast
    if small is None or big is None or small >= 16 or big <= 120:
        return f"peak RSS not the program's own: `true` {small} MB, 128 MB program {big} MB"
    return None


def run_proc(cmd, stdin=None, env=None, cwd=None, timeout=TIMEOUT_S):
    """Runs cmd to completion: returns (metrics dict, stdout bytes). With MEASURE set, wall time,
    CPU time and peak RSS are the measure helper's (the program's own); without it, wall and CPU
    time come from here and peak RSS is None (unknown), never the harness's own size."""
    full_env = {**os.environ, **(env or {})}
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        fd, result = tempfile.mkstemp(prefix="measure-")
        os.close(fd)
        argv = [MEASURE, result, *cmd] if MEASURE else list(cmd)
        inp = open(stdin, "rb") if stdin else subprocess.DEVNULL
        try:
            t = time.perf_counter()
            p = subprocess.Popen(argv, stdin=inp, stdout=out, stderr=err, env=full_env, cwd=cwd)
            timer = threading.Timer(timeout, p.kill)
            timer.start()
            try:
                _, status, ru = os.wait4(p.pid, 0)
            finally:
                timer.cancel()
            wall = time.perf_counter() - t
            line = open(result).read().split() if MEASURE else []
        finally:
            if stdin:
                inp.close()
            os.unlink(result)
        p.returncode = os.waitstatus_to_exitcode(status)
        if p.returncode != 0:
            err.seek(0)
            tail = err.read().decode(errors="replace").strip().splitlines()[-4:]
            raise RunError(f"exit {p.returncode}: {' | '.join(tail)}")
        out.seek(0)
        if MEASURE:
            if len(line) != 4:
                raise RunError("the measure helper wrote no result")
            wall, user, sys_s = int(line[0]) / 1e9, int(line[2]) / 1e6, int(line[3]) / 1e6
            rss = int(line[1]) / 1024  # Linux: KiB (the helper is built and used on Linux only)
        else:
            user, sys_s, rss = ru.ru_utime, ru.ru_stime, None
        metrics = {
            "wall_s": round(wall, 6),
            "user_s": round(user, 4),
            "sys_s": round(sys_s, 4),
            "cpu_s": round(user + sys_s, 4),
            "rss_mb": None if rss is None else round(rss, 2),
        }
        return metrics, out.read()


def _num(s):
    try:
        float(s)
        return True
    except ValueError:
        return False


def parse_output(kind, text):
    """The numbers a program reports itself: {key: (value, checksum)}."""
    found = {}
    if kind == "results":
        for line in text.splitlines():
            p = line.split()
            if len(p) == 5 and p[0] == "RESULT":
                try:
                    ops, ms = int(p[2]), float(p[4])
                except ValueError:
                    continue
                found[p[1]] = (ops / (ms / 1000.0) if ms > 0 else 0.0, f"{ops}/{p[3]}")
    elif kind == "cmap":
        for line in text.splitlines():
            p = line.split()
            if len(p) == 4 and _num(p[2]):
                found[f"{p[0]} x{p[1]}"] = (float(p[2]), p[3])
    elif kind == "named":
        for line in text.splitlines():
            p = line.split()
            if len(p) == 3 and _num(p[1]):
                found[p[0]] = (float(p[1]), p[2])
    elif kind == "fetch":
        for line in text.splitlines():
            m = re.match(r"(\w+): (.*)", line)
            if not m:
                continue
            secs = re.search(r"(\d+\.\d+)s\b", m.group(2))
            sizes = re.findall(r"(\d+) (?:bytes|users)", m.group(2))
            if secs:
                found[m.group(1)] = (float(secs.group(1)), ",".join(sizes))
    return found


def check_output(bench, out, reference, impl, ref_label):
    """None when out is right, else the reason."""
    check = impl.get("check", bench["check"])
    if check == "none":
        return None
    if check.startswith("file:"):
        expected = open(check[5:], "rb").read()
        return None if out == expected else f"output differs from {os.path.basename(check[5:])}"
    if reference is None:
        return None
    if check == "ref":
        return None if out == reference else f"output differs from {ref_label}'s"
    if check == "json":
        try:
            same = json.loads(out) == json.loads(reference)
        except ValueError as e:
            return f"not JSON: {e}"
        return None if same else f"value differs from {ref_label}'s"
    mine = parse_output(check, out.decode(errors="replace"))
    ref = parse_output(check, reference.decode(errors="replace"))
    if not mine:
        return "printed no results"
    missing = [k for k in ref if k not in mine]
    extra = [k for k in mine if k not in ref]
    if missing or extra:
        return (f"results differ from {ref_label}'s: missing {', '.join(missing) or 'none'}, "
                f"extra {', '.join(extra) or 'none'}")
    bad = [k for k, (_, c) in mine.items() if ref[k][1] != c]
    return f"checksums differ from {ref_label}'s: {', '.join(bad)}" if bad else None


def summarize(runs, parse, unit):
    s = {"runs": len(runs)}
    if not runs:
        return s
    walls = sorted(r["wall_s"] for r in runs)
    s["best_wall_s"] = walls[0]
    s["median_wall_s"] = walls[len(walls) // 2]
    s["min_cpu_s"] = min(r["cpu_s"] for r in runs)
    rss = [r["rss_mb"] for r in runs if r.get("rss_mb") is not None]
    s["peak_rss_mb"] = max(rss) if rss else None
    if parse:
        best = {}
        for r in runs:
            for k, v in r.get("reported", {}).items():
                # ops/s and Mops/s: higher is better; seconds and ms: lower is better.
                better = (lambda a, b: a < b) if unit in ("s", "ms") else (lambda a, b: a > b)
                if k not in best or better(v, best[k]):
                    best[k] = v
        s["reported"] = best
    return s


def measure(bench, runs, log=sys.stderr):
    """Runs bench; returns its result record (impls with runs, summaries and failures)."""
    impls = [dict(i) for i in bench["impls"]]
    parse = bench.get("parse")
    reference, ref_label = None, None

    def once(i):
        if i.get("pre"):
            i["pre"]()
        return run_proc(i["cmd"], i.get("stdin"), i.get("env"), i.get("cwd"))

    for i in impls:
        i.setdefault("runs", [])
        if i.get("error"):
            continue
        print(f"[{bench['category']}] {bench['name']}: check {i['label']}", file=log, flush=True)
        try:
            metrics, out = once(i)
        except Exception as e:  # a failed run is a result, never the end of the run
            i["error"] = str(e)
            continue
        if reference is None and bench["check"] != "none" and not bench["check"].startswith("file:"):
            reference, ref_label = out, i["label"]
        reason = check_output(bench, out, reference, i, ref_label)
        if reason:
            i["error"] = f"check failed: {reason}"
        i["check_run"] = metrics
    live = [i for i in impls if not i.get("error")]
    runs *= bench.get("runs_factor", 1)
    for r in range(runs):
        k = r % len(live) if live else 0
        for i in live[k:] + live[:k]:
            if i.get("error"):
                continue
            print(f"[{bench['category']}] {bench['name']}: round {r + 1}/{runs} {i['label']}",
                  file=log, flush=True)
            try:
                metrics, out = once(i)
            except Exception as e:  # a failed run is a result, never the end of the run
                i["error"] = f"round {r + 1}: {e}"
                continue
            if parse:
                metrics["reported"] = {k2: v for k2, (v, _) in
                                       parse_output(parse, out.decode(errors="replace")).items()}
            i["runs"].append(metrics)
    for i in impls:
        i["summary"] = summarize(i["runs"], parse, bench.get("unit", "s"))
        i["cmd"] = " ".join(i["cmd"])
        i.pop("pre", None)
    rec = {k: v for k, v in bench.items() if k != "impls"}
    rec["impls"] = impls
    return rec
