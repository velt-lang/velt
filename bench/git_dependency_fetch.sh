#!/bin/bash
# Cold fetch and build of an app with a git dependency on one package of a repository, per velt
# binary (#3786): how long `velt install` and the `velt build` after it take with an empty cache,
# and what the cache then holds.
#
# usage: bench/git_dependency_fetch.sh <label> <velt binary> <std dir> [rounds] [url] [rev] [path]
#   bench/git_dependency_fetch.sh base ../main/target/release/velt ../main/std 3
#   bench/git_dependency_fetch.sh head target/release/velt std 3
# Run base and head interleaved; the network's share varies from run to run.
# Each round's cache and app go in a directory under $TMPDIR, removed at the end: the whole tree
# of a large repository needs room there.
set -eu
label=$1
velt=$(realpath "$2")
std=$(realpath "$3")
rounds=${4:-3}
url=${5:-https://github.com/velt-lang/velt}
rev=${6:-6b441a1cabf05c0332a0980158d19dcdcc355640}
path=${7:-packages/http-server}
name=$(basename "$path")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for i in $(seq 1 "$rounds"); do
  home=$work/home-$i
  app=$work/app-$i
  mkdir -p "$app/src"
  cat > "$app/package.velt" <<VELT
import type { Package } from "velt/package";

export const pkg: Package = {
  name: "app",
  version: "0.1.0",
  dependencies: { "$name": { git: "$url", rev: "$rev", path: "$path" } },
};
VELT
  printf 'import "%s";\n\nconsole.log("built");\n' "$name" > "$app/src/main.velt"
  start=$(date +%s.%N)
  (cd "$app" && VELT_HOME=$home VELT_STD=$std "$velt" install > "$work/install-$i.log" 2>&1) \
    || { cat "$work/install-$i.log"; exit 1; }
  installed=$(date +%s.%N)
  (cd "$app" && VELT_HOME=$home VELT_STD=$std "$velt" build > "$work/build-$i.log" 2>&1) \
    || { cat "$work/build-$i.log"; exit 1; }
  built=$(date +%s.%N)
  files=$(find "$home/cache/git" -type f | wc -l)
  size=$(du -sm "$home/cache/git" | cut -f1)
  echo "$label $i: install $(echo "$installed - $start" | bc) s, build $(echo "$built - $installed" | bc) s, cache/git $files files, $size MB"
done
