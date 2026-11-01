#!/usr/bin/env bash
# Downloads the JSON benchmark corpora every JSON library reports on (canada.json: GeoJSON,
# floats; citm_catalog.json: wide objects, integers; twitter.json: text, non-ASCII), from
# serde-rs/json-benchmark at a pinned commit (by way of miloyip/nativejson-benchmark), into
# bench/json/corpora/data, and checks their SHA-256. The data is not committed: those
# repositories' licences cover their code, not necessarily the data (twitter.json is Twitter API
# sample output, citm_catalog.json an external dataset), so don't vendor it.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$HERE/data"
sum() { if command -v sha256sum > /dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -c1-64; }
while read -r name hash; do
  f="$HERE/data/$name.json"
  [[ -f $f && $(sum "$f") == "$hash" ]] && continue
  curl -sSfL -o "$f" "https://raw.githubusercontent.com/serde-rs/json-benchmark/17b13dd2d7a5e5fdd5594e847077932f955b5e2b/data/$name.json"
  if [[ $(sum "$f") != "$hash" ]]; then
    echo "$name.json: checksum mismatch" >&2
    rm -f "$f"
    exit 1
  fi
done <<'LIST'
canada f83b3b354030d5dd58740c68ac4fecef64cb730a0d12a90362a7f23077f50d78
citm_catalog a73e7a883f6ea8de113dff59702975e60119b4b58d451d518a929f31c92e2059
twitter a08b769f32b95f426cbc3abafcec65c1a19d3eb544d4ddf320eae142c99efc5d
LIST
