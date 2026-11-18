#!/bin/bash
# vm-alloc-callers.sh <profile-dir>: (in the VM) callgrind's caller tree of the allocator and
# vector growth in <dir>/callgrind-todo_api-units.out, kept as <dir>/alloc-callers.txt.
D=$1
callgrind_annotate --tree=caller --inclusive=yes --threshold=99 \
  "$D/callgrind-todo_api-units.out" > "$D/callgrind-tree-caller.txt" 2>&1
echo "tree lines: $(wc -l < "$D/callgrind-tree-caller.txt")"
