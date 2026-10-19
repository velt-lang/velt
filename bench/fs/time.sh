#!/usr/bin/env bash
# Wall time of commands, for comparing builds of a program (e.g. a directory walk before and
# after moving it to `readDirEntries`): each command runs once to warm the OS caches, then N times
# (default 10), alternating between the commands so machine noise hits them alike; prints the
# best and median time per command. Output goes to /dev/null.
#
#   bench/fs/time.sh [--runs N] -- CMD1 ARGS... [--- CMD2 ARGS... ...]
set -euo pipefail
RUNS=10
if [ "${1:-}" = "--runs" ]; then RUNS=$2; shift 2; fi
[ "${1:-}" = "--" ] || { echo "usage: bench/fs/time.sh [--runs N] -- CMD... [--- CMD...]" >&2; exit 2; }
shift
PYTHON=$(command -v python3 || command -v python)
"$PYTHON" - "$RUNS" "$@" <<'PY'
import subprocess, sys, time
runs, args = int(sys.argv[1]), sys.argv[2:]
cmds, cur = [], []
for a in args:
    if a == "---":
        cmds.append(cur); cur = []
    else:
        cur.append(a)
cmds.append(cur)
for c in cmds:
    subprocess.run(c, check=True, stdout=subprocess.DEVNULL)
times = [[] for _ in cmds]
for _ in range(runs):
    for i, c in enumerate(cmds):
        t = time.perf_counter()
        subprocess.run(c, check=True, stdout=subprocess.DEVNULL)
        times[i].append((time.perf_counter() - t) * 1000)
print("| command | best of %d (ms) | median (ms) |" % runs)
print("|---|---:|---:|")
for c, ts in zip(cmds, times):
    ts.sort()
    print(f"| `{' '.join(c)}` | {ts[0]:.1f} | {ts[len(ts) // 2]:.1f} |")
PY
