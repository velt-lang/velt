#!/bin/bash
# usage: note.sh <task id> <file>   posts the file's text as a crew note
set -eu
crew note "$1" "$(cat "$2")"
