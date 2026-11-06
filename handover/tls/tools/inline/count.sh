#!/bin/bash
# count.sh <exe> <args...>: instructions retired
{ /usr/bin/time -l "$@" >/dev/null; } 2>&1 | awk '/instructions retired/{print $1}'
