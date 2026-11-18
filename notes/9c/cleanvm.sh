#!/bin/bash
# Removes the VM's 9c and #3202 measurement builds (finished; rebuild with build.sh if needed).
rm -rf "$HOME/b9c" "$HOME/b3202"
df -h "$HOME" | tail -1
du -sh "$HOME"/b* 2>/dev/null
