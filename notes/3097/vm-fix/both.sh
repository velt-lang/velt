#!/bin/bash
# Counts NAMES at THREADS: base vs the head the builder measured (5cd66ec71's std), then base vs
# the worktree's std (the fixes).
#   orb -m velt-prof bash notes/3097/vm-fix/both.sh "race_select timers_fire_phases" "1 4" [runs]
set -u
H=$(cd "$(dirname "$0")" && pwd)
echo "## head = 5cd66ec71 (measured on the builder)"
HEAD_STD=$HOME/b3097fix/std-5cd66ec71 bash "$H/count.sh" "$1" "$2" "${3:-1}"
echo "## head = cross-wake worktree (the fixes)"
bash "$H/count.sh" "$1" "$2" "${3:-1}"
