"""Runs the bench/db programs listed by run.sh and prints the Markdown results table.

Usage (from run.sh): measure.py <impls.txt> <runs> <quick 0|1> [filter...]

Each line of impls.txt is `backend|label|command`, where the command is `n/a: <reason>` for an
implementation that can't run. Each program prints `RESULT <backend.workload> <ops> <checksum> <ms>`
lines, timing only its own loop with a monotonic clock. Every program runs once untimed (the
check: op counts and checksums must equal Rust's), then in `runs` rounds of one run per
implementation (interleaved, so load changes on the machine hit all of them alike); a workload
keeps its best ops/s. Peak RSS is the smallest of the timed runs (like bench/benchmarks-game).
"""

import os
import subprocess
import sys
import tempfile
import threading

TIMEOUT_S = 600


def run_once(cmd):
    """Runs cmd; returns ({workload: (ops, checksum, ms)}, peak RSS MB) or raises RuntimeError."""
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        p = subprocess.Popen(cmd, stdout=out, stderr=err, stdin=subprocess.DEVNULL)
        timer = threading.Timer(TIMEOUT_S, p.kill)
        timer.start()
        try:
            # wait4 gives this child's own rusage (peak RSS).
            _, status, ru = os.wait4(p.pid, 0)
        finally:
            timer.cancel()
        if status != 0:
            err.seek(0)
            tail = err.read().decode(errors="replace").strip().splitlines()[-3:]
            raise RuntimeError(f"wait status {status}: {' | '.join(tail)}")
        out.seek(0)
        results = {}
        for line in out.read().decode(errors="replace").splitlines():
            parts = line.split()
            if len(parts) == 5 and parts[0] == "RESULT":
                results[parts[1]] = (int(parts[2]), int(parts[3]), float(parts[4]))
        rss = ru.ru_maxrss / (1 << 20) if sys.platform == "darwin" else ru.ru_maxrss / 1024
        return results, rss


def selected(workload, filters):
    if not filters:
        return True
    return any(workload == f or workload.split(".")[0] == f for f in filters)


def fmt_ops(x):
    return f"{x:,.0f}"


def main():
    listing, runs, quick, filters = sys.argv[1], int(sys.argv[2]), sys.argv[3] == "1", sys.argv[4:]
    backends = {}
    for line in open(listing):
        backend, label, cmd = line.rstrip("\n").split("|", 2)
        backends.setdefault(backend, []).append((label, cmd))

    print("| workload | implementation | ops/s | × Rust |")
    print("|---|---|---:|---:|")
    notes, memory = [], []
    for backend, impls in backends.items():
        order, rows = [], {}
        runnable = []
        for label, cmd in impls:
            if cmd.startswith("n/a"):
                notes.append(f"{backend} {label if label != 'all' else ''}: {cmd[5:]}".replace("  ", " "))
            else:
                runnable.append((label, cmd.split() + (["quick"] if quick else [])))
        # The untimed checking run of every implementation (Rust first: the reference).
        checks = {}
        for label, args in runnable:
            print(f"checking {backend}: {label}...", file=sys.stderr)
            try:
                checks[label], _ = run_once(args)
            except RuntimeError as e:
                notes.append(f"{backend} {label}: {e}")
        reference = checks.get("Rust", {})
        bad = {label: set() for label in checks}
        for label, check in checks.items():
            order += [w for w in check if w not in order]
            for w, (ops, checksum, _) in check.items():
                ref = reference.get(w)
                if ref is not None and (ref[0], ref[1]) != (ops, checksum):
                    bad[label].add(w)
                    notes.append(f"{w} {label}: ops/checksum {ops}/{checksum} != Rust's {ref[0]}/{ref[1]}")
        # Timed rounds: one run of every implementation per round, so load changes on the
        # machine hit all of them alike.
        best = {label: {} for label in checks}
        rss = {label: float("inf") for label in checks}
        for r in range(runs):
            for label, args in runnable:
                if label not in best:
                    continue
                print(f"round {r + 1}/{runs} {backend}: {label}...", file=sys.stderr)
                try:
                    results, run_rss = run_once(args)
                except RuntimeError as e:
                    notes.append(f"{backend} {label} (round {r + 1}): {e}")
                    continue
                rss[label] = min(rss[label], run_rss)
                for w, (ops, _, ms) in results.items():
                    best[label][w] = max(best[label].get(w, 0.0), ops / (ms / 1000.0))
        for label in best:
            rows[label] = {w: (None if w in bad[label] else v) for w, v in best[label].items()}
            if rss[label] != float("inf"):
                memory.append(f"| {backend} | {label} | {rss[label]:.1f} |")
        rust = rows.get("Rust") or {}
        for w in order:
            if not selected(w, filters):
                continue
            for label, _ in impls:
                if label == "all":
                    continue
                ops = (rows.get(label) or {}).get(w)
                if ops is None:
                    print(f"| {w} | {label} | n/a | |")
                    continue
                ratio = f"{ops / rust[w]:.2f}" if rust.get(w) else ""
                print(f"| {w} | {label} | {fmt_ops(ops)} | {ratio} |")
        if not order:
            for label, _ in impls:
                print(f"| {backend} | {label if label != 'all' else 'all'} | n/a | |")
    print()
    size = "quick (1/20)" if quick else "full"
    print(f"Best of {runs} runs, {size} sizes. × Rust = ops/s ÷ Rust's ops/s (below 1 is slower).")
    for n in notes:
        print(f"- n/a: {n}")
    if memory:
        print()
        print("| program | implementation | peak RSS MB (whole run) |")
        print("|---|---|---:|")
        print("\n".join(memory))


if __name__ == "__main__":
    main()
