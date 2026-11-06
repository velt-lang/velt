#!/bin/bash
# instr.sh <cmd...>: best-of-3 "instructions retired" of a command.
best=""
for i in 1 2 3; do
  v=$( { /usr/bin/time -l "$@" >/dev/null 2>/dev/null; } 2>&1 | awk '/instructions retired/{print $1}')
  v=$( /usr/bin/time -l "$@" 2>&1 >/dev/null | awk '/instructions retired/{print $1}')
  if [ -z "$best" ] || [ "$v" -lt "$best" ]; then best=$v; fi
done
echo $best
