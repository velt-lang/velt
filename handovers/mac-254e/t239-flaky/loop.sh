#!/bin/bash
# usage: loop.sh <exe> <want-file> <runs> <threads> [VAR=value...]
# Runs <exe> <runs> times at VELT_THREADS=<threads>, 6 at once; prints the failure count and the
# distinct wrong outputs (stdout, and whether every block was freed).
exe=$1; want=$2; runs=$3; th=$4; shift 4
export LOOP_EXE=$exe LOOP_OUT=$(mktemp -d) VELT_THREADS=$th
for kv in "$@"; do export "$kv"; done
one() {
  "$LOOP_EXE" > "$LOOP_OUT/$1.out" 2> "$LOOP_OUT/$1.err"
  echo $? > "$LOOP_OUT/$1.code"
}
export -f one
seq 1 "$runs" | xargs -P ${LOOP_P:-6} -n 1 bash -c 'one "$0"'
bad=0
: > "$LOOP_OUT/bad.txt"
for i in $(seq 1 "$runs"); do
  leak=$(grep -o 'blocks=[0-9]*/[0-9]*' "$LOOP_OUT/$i.err" | awk -F'[=/]' '$2 != $3 {print "LEAK " $0}')
  if ! cmp -s "$LOOP_OUT/$i.out" "$want" || [ "$(cat "$LOOP_OUT/$i.code")" != 0 ] || [ -n "$leak" ]; then
    bad=$((bad + 1))
    echo "code $(cat "$LOOP_OUT/$i.code") $leak: $(tr '\n' '|' < "$LOOP_OUT/$i.out")" >> "$LOOP_OUT/bad.txt"
  fi
done
echo "T$th: $bad/$runs bad"
sort "$LOOP_OUT/bad.txt" | sed 's/blocks=[0-9]*\/[0-9]*/blocks=*/' | uniq -c | head -5
rm -rf "$LOOP_OUT"
