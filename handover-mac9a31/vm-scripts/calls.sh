#!/bin/bash
# Inside the VM: who calls the runtime lock's functions, and how often (the callgrind profile of
# lines-head-g from profile.sh).
set -u
callgrind_annotate --tree=caller "$HOME/stdin-out/cg.out" 2>/dev/null \
  | grep -B6 -E '\* .*(lock_slow|unlock_slow|velt_rt_mutex_lock|velt_rt_mutex_unlock)' | cut -c1-160 | head -50
