#!/bin/bash
# One loop for all trains, light on the API: every 5 minutes, one status call per open train.
# Green -> train-land. Failed once -> rerun failed jobs. Failed after a rerun -> report and drop.
# Usage: trains.sh run:train [run:train ...]   (a run already on attempt 2 counts as rerun)
here=$(cd "$(dirname "$0")" && pwd); cd "$(git -C "$here" rev-parse --show-toplevel)"
declare -A TRAIN=() RERUN=() DONE=()
for p in "$@"; do TRAIN[${p%%:*}]=${p##*:}; done
# A Monitor's expiry doesn't kill this script: stop on our own after 29 minutes, so restarts
# never leave copies that land and re-run trains twice.
end=$(( $(date +%s) + 29*60 ))
while [ ${#DONE[@]} -lt ${#TRAIN[@]} ]; do
  [ "$(date +%s)" -ge "$end" ] && { echo "trains.sh: 29 min up, exiting (re-arm)"; exit 0; }
  rem=$(gh api rate_limit --jq .resources.core.remaining 2>/dev/null </dev/null || echo 0)
  if [ "${rem:-0}" -lt 300 ]; then sleep 300; continue; fi
  for run in "${!TRAIN[@]}"; do
    [ -n "${DONE[$run]:-}" ] && continue
    st=$(gh run view "$run" --json status,conclusion,attempt --jq '"\(.status) \(.conclusion) \(.attempt)"' 2>/dev/null </dev/null) || continue
    set -- $st
    [ "$1" = completed ] || continue
    t=${TRAIN[$run]}
    if [ "$2" = success ]; then
      out=$(bash "$here/train-land.sh" "$t" 2>&1 </dev/null | tail -3 | tr '\n' ' ')
      echo "LANDED? $t: $out"; echo "trains.sh $t: $out" >> "${VELT_TRAINS_DIR:-$HOME/.velt-trains}/log.txt"
      DONE[$run]=1
    elif [ -z "${RERUN[$run]:-}" ] && [ "${3:-1}" -lt 2 ]; then
      gh run rerun "$run" --failed </dev/null >/dev/null 2>&1 && echo "RERUN $t ($run)"
      RERUN[$run]=1
    else
      echo "FAILED $t ($run) after rerun: $2"
      DONE[$run]=1
    fi
  done
  sleep 300
done
echo "all trains handled"
