#!/bin/bash
# Posts "heavy done" and the #3273 result once the hub answers; gives up after 30 minutes.
for i in $(seq 1 90); do
  if crew send all "heavy done mac-4d96 (#3273 Mac checks green, pushed c1264fc4d; slot B free)" 2>/dev/null | grep -q sent; then
    crew note 135 "mac-4d96: #3273 pushed c1264fc4d (the five cuts + the new test, cherry-picked on 0205e7bc1). Mac checks on main 4e8bc7f1f: build, test file 8/8, scheduler suites + c_export_async 254/254, sleep/timer goldens, precheck. (First suite run: the two std_scheduler_all joins failed because their first child programs never started, stuck at process start at 0% CPU while I had lldb running; the program passed 12/12 by hand and the suite 4/4 on rerun.) 'Since the approval' comment posted with cut 5 called out, both architectures' rows and the spawn_many T4 median: https://github.com/velt-lang/velt/pull/3273#issuecomment-7043767638 . Ready for re-review; lands after #3322." > /dev/null 2>&1
    crew status "#135: #3273 pushed c1264fc4d (re-review); #3202 fix held locally (unproven); #3323 prototype not ready (an ICE under widened timing to explain)" > /dev/null 2>&1
    echo "posted after $i tries"; exit 0
  fi
  sleep 20
done
echo "hub unreachable for 30 min"; exit 1
