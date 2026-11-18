#!/bin/bash
# Release LLVM IR (`--emit llvm`) of bench NAME with base std and with HEAD_STD (default the
# worktree's), in the VM: $HOME/b3097fix/<name>-{base,head}.ll
#   orb -m velt-prof bash notes/3097/vm-fix/emit.sh race_select
set -u
OUT=$HOME/b3097fix
WT=/Users/andii/dev/velt/branches/cross-wake
V=$OUT/tgt/release/velt
name=$1
src=$WT/bench/async/$name.velt
for s in base head; do
  std=$OUT/base-std; [ $s = head ] && std=${HEAD_STD:-$WT/std}
  VELT_STD=$std "$V" build --release --backend llvm --emit llvm "$src" >"$OUT/$name-$s.ll" 2>"$OUT/$name-$s.ll.err"
  echo "$s: $(wc -l <"$OUT/$name-$s.ll") lines"
done
