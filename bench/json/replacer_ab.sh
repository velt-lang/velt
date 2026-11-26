#!/usr/bin/env bash
# Instructions of bench/json/replacer.velt (--release, cachegrind) with this tree's walk of
# `JSON.stringify(value, replacer)` and with the walk it replaced (#3774): every `toJSON` run
# while the value is written as text, which is parsed and then replaced. The same compiler builds
# both programs; only std/lang/hooks/json_replace.velt's `jsonStringifyReplaced` is swapped. Linux
# only (valgrind).
#
#   bench/json/replacer_ab.sh VELT WORKDIR
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VELT=$1
WORK=$2
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
rm -rf "$WORK/old-std"
cp -r "$ROOT/std" "$WORK/old-std"
F=$WORK/old-std/lang/hooks/json_replace.velt
perl -0pi -e 's/  return replaceJsonTop<T>\(value, replacer\);/  let root = JsonValue.of(null);\n  try {\n    root = JsonValue.parse(JSON.stringify(value), { maxDepth: 4294967295 });\n  } catch (e) {\n    panic("JSON.stringify wrote text it cannot read back");\n  }\n  return jsonReplacedText(jsonReplaced("", root, replacer, 0));/' "$F"
perl -0pi -e 's/(import \{ replaceJsonTop \} from "velt\/encoding\/json\/replaced";)/$1\nimport { jsonReplaced, jsonReplacedText } from "velt\/encoding\/json\/replace_ops";\nimport { JSON } from "velt\/prelude\/json\/json";/' "$F"
grep -q 'jsonReplaced("", root' "$F" || { echo "the swap did not apply" >&2; exit 1; }
for side in old new; do
  std=$ROOT/std
  [ "$side" = old ] && std=$WORK/old-std
  VELT_STD=$std "$VELT" build --release "$ROOT/bench/json/replacer.velt" -o "$WORK/replacer-$side" >/dev/null
  out=$("$WORK/replacer-$side")
  n=$(valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file="$WORK/cg-$side.out" \
      "$WORK/replacer-$side" 2>&1 >/dev/null | sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,)
  printf '%s\t%s\t%s\n' "$side" "$n" "$out"
done
