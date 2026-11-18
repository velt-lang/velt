#!/bin/bash
# Experiment: one `@inline` mark at a time in copies of the worktree's std: head-std-box (sched.velt
# `schedBox`) and head-std-rel (sched_node.velt `release`).
#   orb -m velt-prof bash notes/3097/vm-fix/mark1.sh
set -eu
OUT=$HOME/b3097fix
WT=/Users/andii/dev/velt/branches/cross-wake
rm -rf "$OUT/head-std-box" "$OUT/head-std-rel"
cp -r "$WT/std" "$OUT/head-std-box"
cp -r "$WT/std" "$OUT/head-std-rel"
sed -i 's/^export function schedBox(/@inline\nexport function schedBox(/' "$OUT/head-std-box/lang/hooks/sched.velt"
sed -i 's/^export function release(n: usize): void {/@inline\nexport function release(n: usize): void {/' "$OUT/head-std-rel/lang/hooks/sched_node.velt"
