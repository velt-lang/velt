#!/bin/bash
# Stops only my allocator A/B processes in the VM: those whose environment or arguments name
# tgt-alloc- (my target directories). Never a process group.
mine=""
for p in $(pgrep -u "$(id -u)" .); do
  if tr '\0' '\n' < /proc/$p/environ 2>/dev/null | grep -q "tgt-alloc-" || tr '\0' ' ' < /proc/$p/cmdline 2>/dev/null | grep -q "tgt-alloc-\|alloc-ab.sh"; then
    mine="$mine $p"
  fi
done
echo "stopping:$mine"
for p in $mine; do
  echo "  $p $(tr '\0' ' ' < /proc/$p/cmdline 2>/dev/null | cut -c1-90)"
done
[ -n "$mine" ] && kill $mine 2>/dev/null
sleep 3
left=""
for p in $(pgrep -u "$(id -u)" .); do
  tr '\0' '\n' < /proc/$p/environ 2>/dev/null | grep -q "tgt-alloc-" && left="$left $p"
done
echo "left:$left"
