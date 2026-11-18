#!/bin/bash
# The 9c count.sh on the #3202 compiler ($HOME/b3202): base std = main's, head std = HEAD_STD
# (default: the cross-wake worktree's, uncommitted edits included), sources from cross-wake.
#   orb -m velt-prof bash notes/3202/count.sh "timers_due_stored timers_due_reversed" "4" 3
export HEAD_WT=/Users/andii/dev/velt/branches/cross-wake
export VELT=$HOME/b3202/tgt/release/velt
export BASE_STD=$HOME/b3202/base-std
export HEAD_STD=${HEAD_STD:-$HEAD_WT/std}
export B9C_OUT=$HOME/b3202
exec bash "$(dirname "$0")/../9c/count.sh" "$@"
