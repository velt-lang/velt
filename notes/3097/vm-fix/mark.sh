#!/bin/bash
# Experiment: `@inline` on sched.velt `schedBox` and sched_node.velt `release`, in a copy of base std
# ($HOME/b3097fix/base-std-marked) and in a copy of the worktree's std (head-std-marked).
#   orb -m velt-prof bash notes/3097/vm-fix/mark.sh
set -eu
OUT=$HOME/b3097fix
WT=/Users/andii/dev/velt/branches/cross-wake
for s in base head; do
  src=$OUT/base-std; [ $s = head ] && src=$WT/std
  dst=$OUT/$s-std-marked
  rm -rf "$dst" && cp -r "$src" "$dst"
  sed -i 's/^export function schedBox(/@inline\nexport function schedBox(/' "$dst/lang/hooks/sched.velt"
  sed -i 's/^export function release(n: usize): void {/@inline\nexport function release(n: usize): void {/' "$dst/lang/hooks/sched_node.velt"
  grep -c "^@inline" "$dst/lang/hooks/sched.velt" "$dst/lang/hooks/sched_node.velt"
done
