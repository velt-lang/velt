#!/usr/bin/env sh
# Write the index of releases the velt launcher reads (velt_toolchain::release, format 1):
#
#   { "format": 1, "generated": <unix seconds>, "launcher": "<newest stable version>",
#     "releases": [ { "version": "0.1.1" }, { "version": "0.1.2", "yanked": "<why>" } ] }
#
# Usage: scripts/release-index.sh <releases> <yanks.json> [<generated>]
#   <releases>    one JSON object per line, {"tag": "v0.1.1", "draft": false, "assets": [...]}:
#                 gh api --paginate repos/<owner>/<repo>/releases \
#                   --jq '.[] | {tag: .tag_name, draft, assets: [.assets[].name]}'
#   <yanks.json>  {"<version>": "<why it was yanked>"} (.github/release-yanks.json)
#   <generated>   the time to write (default: now)
#
# Lists the published releases tagged v<semver> that the launcher can install: those with a
# signed SHA256SUMS (SHA256SUMS.sig). The release workflow signs the result and uploads it to
# the `index` release (.github/workflows/release-index.yml).
set -eu

if [ $# -lt 2 ]; then
    echo "usage: $0 <releases> <yanks.json> [<generated>]" >&2
    exit 2
fi
generated=${3:-$(date +%s)}

jq -s --slurpfile yanks "$2" --argjson generated "$generated" '
  ($yanks[0] // {}) as $yanked
  | [ .[]
      | select(.draft | not)
      | select(.tag | test("^v[0-9]+\\.[0-9]+\\.[0-9]+(-[0-9A-Za-z.-]+)?$"))
      | select(.assets | index("SHA256SUMS.sig"))
      | .tag[1:] ]
  | unique as $versions
  | ($versions
      | map(select(test("-") | not) | select($yanked[.] == null))
      | sort_by(split(".") | map(tonumber))
      | last) as $launcher
  | { format: 1, generated: $generated }
    + (if $launcher then { launcher: $launcher } else {} end)
    + { releases: [ $versions[]
                    | { version: . } + (if $yanked[.] then { yanked: $yanked[.] } else {} end) ] }
' "$1"
