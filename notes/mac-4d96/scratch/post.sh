#!/bin/bash
# Posts a note on crew #135 and a short message to the coordinator once the hub answers; gives
# up after 20 minutes.   bash post.sh <note-file> <message-file>
for i in $(seq 1 60); do
  if crew note 135 "$(cat "$1")" 2>/dev/null | grep -q noted; then
    until crew send win-coordinator "$(cat "$2")" 2>/dev/null | grep -q sent; do sleep 20; done
    echo "posted after $i tries"
    exit 0
  fi
  sleep 20
done
echo "hub still unreachable after 20 min"
exit 1
