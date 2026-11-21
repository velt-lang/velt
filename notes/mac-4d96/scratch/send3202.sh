#!/bin/bash
# Posts the #3202 note and the message once the hub answers (it was unreachable); gives up after 20 min.
S=$(cd "$(dirname "$0")" && pwd)
for i in $(seq 1 60); do
  if crew note 135 "$(cat "$S/note-3202.md")" 2>/dev/null | grep -q noted; then
    until crew send win-coordinator "mac-4d96: #3202 idea on crew #135: no park exit lacks retiring(e), but a retire ask that finds the spare's park word down (awake) is never answered and never re-asked (main too; #3202's spin widens the window, and the ask loop over 4 spares makes it likely). Proposed fix: park's test with the word up also reads E_RETIRED. Tell me which branch." 2>/dev/null | grep -q sent; do sleep 20; done
    echo "posted after $i tries"
    exit 0
  fi
  sleep 20
done
echo "hub still unreachable after 20 min"
exit 1
