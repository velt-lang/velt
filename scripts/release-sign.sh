#!/usr/bin/env sh
# Sign release files with the velt release key (#948): each <file> gets <file>.sig, the 64-byte
# Ed25519 signature of its bytes, which velt_toolchain::signature and get-velt.sh check. Each
# signature is then checked with the public key velt is built with
# (velt_toolchain::signature::RELEASE_PUBLIC_KEY), so a signing key that doesn't match it fails
# here, before anything is published.
#
# Usage: VELT_RELEASE_SIGNING_KEY=<PEM private key> scripts/release-sign.sh <file>...
#        scripts/release-sign.sh --public-key     (prints the built-in public key, PEM)
# Needs OpenSSL 3.0 or newer (`pkeyutl -rawin`).
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
hex=$(sed -n 's/^ *"\([0-9a-f]\{64\}\)";$/\1/p' "$repo/crates/velt_toolchain/src/signature.rs")
if [ "${#hex}" != 64 ]; then
    echo "error: cannot read RELEASE_PUBLIC_KEY from crates/velt_toolchain/src/signature.rs" >&2
    exit 1
fi
# An Ed25519 public key's DER is a fixed 12-byte header and the 32-byte key.
der=$(printf '302a300506032b6570032100%s' "$hex" | awk '{
    h = "0123456789abcdef"
    for (i = 1; i < length($0); i += 2)
        printf "\\%03o", (index(h, substr($0, i, 1)) - 1) * 16 + index(h, substr($0, i + 1, 1)) - 1
}')
# shellcheck disable=SC2059
public_key=$(printf -- '-----BEGIN PUBLIC KEY-----\n%s\n-----END PUBLIC KEY-----' \
    "$(printf "$der" | openssl base64 -A)")
if [ "${1:-}" = --public-key ]; then
    printf '%s\n' "$public_key"
    exit 0
fi

if [ $# -eq 0 ]; then
    echo "usage: $0 <file>...   (with VELT_RELEASE_SIGNING_KEY set)" >&2
    exit 2
fi
if [ -z "${VELT_RELEASE_SIGNING_KEY:-}" ]; then
    echo "error: VELT_RELEASE_SIGNING_KEY (the release signing key, PEM) is not set" >&2
    exit 1
fi
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
umask 077
printf '%s\n' "$VELT_RELEASE_SIGNING_KEY" > "$tmp/key.pem"
printf '%s\n' "$public_key" > "$tmp/public.pem"
for f in "$@"; do
    openssl pkeyutl -sign -rawin -inkey "$tmp/key.pem" -in "$f" -out "$f.sig"
    if ! openssl pkeyutl -verify -rawin -pubin -inkey "$tmp/public.pem" -in "$f" -sigfile "$f.sig" >/dev/null 2>&1; then
        rm -f "$f.sig"
        echo "error: VELT_RELEASE_SIGNING_KEY is not the private half of the release key velt is built with" >&2
        exit 1
    fi
    echo "signed $f"
done
