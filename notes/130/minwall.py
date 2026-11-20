"""minwall.py <label> <cmd...>: run the command 15 times in one process; print min and median wall ms."""
import statistics, subprocess, sys, time

label, cmd = sys.argv[1], sys.argv[2:]
times = []
for _ in range(15):
    t0 = time.perf_counter()
    r = subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    times.append((time.perf_counter() - t0) * 1000)
    if r.returncode != 0:
        print(f"{label}: FAILED {r.stderr.decode()[:160]!r}")
        sys.exit(0)
print(f"{label:28s} min {min(times):7.1f} ms  median {statistics.median(times):7.1f} ms")
