#!/bin/bash
# Whether `perf stat` can count instructions in this VM (and why not).
perf stat -x, -e instructions:u true 2>&1 | tail -5
cat /proc/sys/kernel/perf_event_paranoid
