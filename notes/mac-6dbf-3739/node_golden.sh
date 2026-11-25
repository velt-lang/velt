#!/bin/bash
# node_golden.sh <golden.velt> <out>: runs a golden's source under Node 22 with the yield prelude
# (`yieldNow` as `setImmediate`, `sleep` as `setTimeout`), calling `main()`.
set -u
S="/private/tmp/claude-501/-Users-andii-dev-velt-main/6dbf5d3c-1862-4ea4-a154-094b5a8b6b53/scratchpad"
D="$S/nodegold"
mkdir -p "$D"
{
  echo 'const yieldNow = (): Promise<void> => new Promise<void>((r) => setImmediate(r));'
  echo 'const sleep = (ms: number): Promise<void> => new Promise<void>((r) => setTimeout(r, ms));'
  cat "$1"
  echo 'main();'
} > "$D/prog.ts"
cd "$D" && python3 "$S/tmo.py" 180 npx -y node@22 --experimental-strip-types prog.ts > "$2" 2> "$D/err.txt"
echo "node exit $?"
grep -v ExperimentalWarning "$D/err.txt" | grep -v "^(Use" | head -5
