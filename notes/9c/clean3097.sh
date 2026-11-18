#!/bin/bash
# Removes the VM's #3097 fix measurement dir (the builder measures the stack now).
rm -rf "$HOME/b3097fix"
df -h "$HOME" | tail -1
du -sh "$HOME"/b* 2>/dev/null
