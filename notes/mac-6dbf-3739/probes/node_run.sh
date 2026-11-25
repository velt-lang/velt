#!/bin/bash
# Run a probe under Node 22 with yieldNow = setImmediate, spawn = identity.
set -u
D="$(cd "$(dirname "$0")" && pwd)"
f="$D/$1.velt"
t="$D/node_$1.ts"
{
  echo 'const yieldNow = () => new Promise<void>((r) => setImmediate(r));'
  echo 'const spawn = <T,>(p: Promise<T>): Promise<T> => p;'
  cat "$f"
  echo 'main();'
} > "$t"
python3 "$D/../tmo.py" 60 npx -y node@22 --experimental-strip-types "$t" > "$D/node_$1.out" 2>&1
echo "node exit $?"
