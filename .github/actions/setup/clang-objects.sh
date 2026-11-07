#!/usr/bin/env bash
# The objects a merge queue job added to clang's cache, for the push to main that follows: a run on
# a queue branch can save no cache another branch restores (ci.yml's `main` job saves them).
#   clang-objects.sh list         note what the job restored (setup does this)
#   clang-objects.sh pack <tar>   pack what was added since, if anything
#   clang-objects.sh unpack <dir> add the objects in every tar under <dir> to the cache
# Only `large/`, a file per object: small entries live in packs that can't be merged (1.8% of the
# bytes), and without them a job only compiles those again.
set -euo pipefail

before="${RUNNER_TEMP:?}/clang-objects.before"
# An object's path (crates/velt_build_cache/src/files.rs): a key in hex, cut after 2 digits.
# Anything else in those directories is a temporary file or a lock.
object='^target/velt-build-cache/v[0-9]+/llvm/large/[0-9a-f]{2}/[0-9a-f]{62}$'
objects() {
  # A restore that found nothing leaves no such directory: that must not fail the job.
  { find target/velt-build-cache -path '*/llvm/large/*' -type f 2>/dev/null || true; } |
    { grep -E "$object" || true; } | LC_ALL=C sort
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
  unpack)
    # The tars come from a queue run, where anything the change runs can write: every member of
    # every tar must be an object, a regular file, or nothing is unpacked. `tar -t` writes a name
    # with a newline as `\n`, so a line is a member. Objects are only added, never replaced.
    mapfile -t tars < <(find "$2" -type f -name '*.tar')
    for tar in "${tars[@]}"; do
      names=$(tar -tf "$tar")
      types=$(tar -tvf "$tar" | cut -c1)
      if grep -qvE "$object" <<< "$names" || grep -qv '^-$' <<< "$types"; then
        echo "::error::$tar holds something other than objects: nothing unpacked"
        exit 1
      fi
    done
    for tar in "${tars[@]}"; do tar -xf "$tar" --skip-old-files; done
    echo "${#tars[@]} tars unpacked"
    ;;
  *) echo "usage: clang-objects.sh list | pack <tar> | unpack <dir>" >&2; exit 2 ;;
esac
