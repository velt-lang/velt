#!/usr/bin/env sh
# Write the index of releases the velt launcher reads (velt_toolchain::release, format 1):
#
#   { "format": 1, "generated": <unix seconds>, "launcher": "<newest stable version>",
#     "revoked_keys": [ "<hex>" ],
#     "releases": [ { "version": "0.1.1" }, { "version": "0.1.2", "yanked": "<why>" } ] }
#
# Usage: scripts/release-index.sh <releases> <yanks.json> <revoked.json> [<generated>]
#   <releases>    one JSON object per line, {"tag": "v0.1.1", "draft": false, "assets": [...]}:
#                 gh api --paginate repos/<owner>/<repo>/releases \
#                   --jq '.[] | {tag: .tag_name, draft, assets: [.assets[].name]}'
#   <yanks.json>  per version, why it was yanked, or an object with `yanked`, `advisory` (a
#                 problem users of it are told about) and `security` (.github/release-yanks.json):
#                 {"0.2.1": "miscompiles closures", "0.2.0": {"advisory": "…", "security": true}}
#   <revoked.json> ["<release public key, hex>"]: keys that leaked (.github/release-revoked-keys.json;
#                 launchers honour it only in an index signed by a later key: the backup)
#   <generated>   the time to write (default: now)
#
# Lists the published releases tagged v<semver> that the launcher can install: those with a
# signed SHA256SUMS (SHA256SUMS.sig). The release workflow signs the result and uploads it to
# the `index` release (.github/workflows/release-index.yml).
set -eu

if [ $# -lt 3 ]; then
    echo "usage: $0 <releases> <yanks.json> <revoked.json> [<generated>]" >&2
    exit 2
fi
generated=${4:-$(date +%s)}

jq -s --slurpfile yanks "$2" --slurpfile revoked "$3" --argjson generated "$generated" '
  # Per version: a string is why it was yanked; an object may say `yanked`, `advisory` (a problem
  # users of it are told about), both strings, and `security` (a boolean). Anything else is an
  # error here, so a typo fails the workflow instead of publishing an index launchers refuse.
  def note($v):
    if type == "string" then { yanked: . }
    elif type != "object" then
      error("\($v): \(tojson) is not a string (why it was yanked) or an object")
    elif (keys - ["yanked", "advisory", "security"]) != [] then
      error("\($v): unknown fields \(keys - ["yanked", "advisory", "security"] | join(", ")); the fields are yanked, advisory and security")
    elif has("yanked") and (.yanked | type) != "string" then
      error("\($v): `yanked` is \(.yanked | tojson), not a string (why it was yanked)")
    elif has("advisory") and (.advisory | type) != "string" then
      error("\($v): `advisory` is \(.advisory | tojson), not a string")
    elif has("security") and (.security | type) != "boolean" then
      error("\($v): `security` is \(.security | tojson), not true or false")
    else . end;
  ($yanks[0] // {}) as $given
  | if ($given | type) != "object" then error("the yanks file is not an object") else . end
  | ($given | to_entries | map(.key as $v | .value |= note($v)) | from_entries) as $notes
  | ($notes | map_values(.yanked) | with_entries(select(.value != null))) as $yanked
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
    + (($revoked[0] // []) as $keys
       | if ($keys | length) > 0 then { revoked_keys: $keys } else {} end)
    + { releases: [ $versions[]
                    | { version: . } + ($notes[.] // {}) ] }
' "$1"
