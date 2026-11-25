import subprocess, sys

# tmo.py <seconds> <cmd...>: run cmd, kill it after <seconds>; exit 124 on timeout.
secs = float(sys.argv[1])
p = subprocess.Popen(sys.argv[2:])
try:
    sys.exit(p.wait(timeout=secs))
except subprocess.TimeoutExpired:
    p.kill()
    p.wait()
    print(f"timed out after {secs:.0f} s")
    sys.exit(124)
