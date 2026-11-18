#!/bin/bash
# Stops what repro.sh left running in the VM (its script and test binaries), by PID, and removes
# its build dirs.
for p in $(pgrep -f "notes/3174/repro.sh|r3174/head/debug/deps/std_scheduler_rescue|r3174/main/debug/deps/std_scheduler_rescue"); do
  kill "$p" 2>/dev/null && echo "stopped $p"
done
rm -rf "$HOME/r3174"
df -h "$HOME" | tail -1
du -sh "$HOME"/* 2>/dev/null | sort -h | tail -5
