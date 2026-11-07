#!/usr/bin/env bash
# The objects a merge queue job added to clang's cache, for the push to main that follows: a run on
# a queue branch can save no cache another branch restores (ci.yml's `main` job saves them).
#   clang-objects.sh list         note what the job restored (setup does this)
#   clang-objects.sh pack <tar>   pack what was added since, if anything
# Only `large/`, a file per object: small entries live in packs that can't be merged (1.8% of the
# bytes), and without them a job only compiles those again.
set -euo pipefail

before="${RUNNER_TEMP:?}/clang-objects.before"
objects() {
  # A restore that found nothing leaves no such directory: that must not fail the job.
  { find target/velt-build-cache -path '*/llvm/large/*' -type f 2>/dev/null || true; } | LC_ALL=C sort
}

case "${1:-}" in
  list) objects > "$before" ;;
  pack)
    [ -f "$before" ] || { echo "nothing noted: no restored cache to compare with"; exit 0; }
    new="$RUNNER_TEMP/clang-objects.new"
    objects | LC_ALL=C comm -13 "$before" - > "$new"
    echo "$(wc -l < "$new") objects added"
    if [ -s "$new" ]; then tar -cf "$2" -T "$new"; fi
    ;;
  *) echo "usage: clang-objects.sh list | pack <tar>" >&2; exit 2 ;;
esac
