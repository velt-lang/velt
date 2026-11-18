#!/bin/bash
# The previous #3202 head's std (afbe7bafe) into $HOME/b3202/old-std, then the reviewer's two rows
# and timers_due_join counted base vs that std.
set -eu
OUT=$HOME/b3202
rm -rf "$OUT/old-tmp" "$OUT/old-std" && mkdir -p "$OUT/old-tmp"
git -C /Users/andii/dev/velt/branches/cross-wake archive afbe7bafe std | tar -x -C "$OUT/old-tmp"
mv "$OUT/old-tmp/std" "$OUT/old-std" && rmdir "$OUT/old-tmp"
HEAD_STD=$OUT/old-std bash "$(dirname "$0")/count.sh" "timers_due_stored timers_due_reversed timers_due_join" "4" 3
