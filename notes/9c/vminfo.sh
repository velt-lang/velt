#!/bin/bash
# Free disk and the build dirs in the VM's home.
df -h "$HOME" | tail -1
du -sh "$HOME"/b* 2>/dev/null
