#!/usr/bin/env sh
# Download and install a released Velt toolchain on Linux or macOS.
#
#   curl -fsSL https://github.com/velt-lang/velt/releases/latest/download/get-velt.sh | sh
#   curl -fsSL https://github.com/velt-lang/velt/releases/latest/download/get-velt.sh | sh -s -- --version 0.1.1
#
# Versions install side by side (#948, docs/tooling/platforms.md): the toolchain goes into
# <root>/toolchains/<version>/, and the launcher into <root>/bin/velt, the one directory on PATH.
# It runs the version each package pins (`velt` in package.vlt), else the default. Running this
# again with another version adds it beside the others.
#
# Options (environment variable in parentheses):
#   --version <v>     the release to install, e.g. 0.1.1 (VELT_INSTALL_VERSION); default: the release
#                     this script was published with, or the latest release
#   --prefix <dir>    the root (VELT_INSTALL_PREFIX); default: ~/.velt
#   --default         make this version the default (the first one installed always is)
#   --force           reinstall a version that is already installed
#   --archive <file>  install a downloaded velt-<version>-<target>.tar.gz instead of downloading
#   --no-modify-path  do not add <root>/bin to PATH in your shell profiles
# VELT_INSTALL_BASE_URL replaces https://github.com/velt-lang/velt (forks, mirrors, tests), and
# VELT_INSTALL_PUBLIC_KEY (64 hex digits) the release key, for another build's releases.
#
# A download is checked against the release's SHA256SUMS, and that file against its signature
# (SHA256SUMS.sig) with the velt release key when OpenSSL can check Ed25519 signatures (OpenSSL
# 1.1.1 or newer; not macOS's LibreSSL). The installed launcher checks every later download itself.
set -eu

# The release workflow replaces this with the version it publishes.
published_version="@VELT_RELEASE_VERSION@"

base_url=${VELT_INSTALL_BASE_URL:-https://github.com/velt-lang/velt}
version=${VELT_INSTALL_VERSION:-}
prefix=${VELT_INSTALL_PREFIX:-"$HOME/.velt"}
archive=
modify_path=1
make_default=0
force=0

# The velt release key (velt_toolchain::signature::RELEASE_PUBLIC_KEY), as OpenSSL reads it.
release_key='-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAlXBDOAeP85PjjZH6CzVAe++R8saiPgF4bNYBUQBhBcs=
-----END PUBLIC KEY-----'


say() { printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || err "--version needs a value"; version=$2; shift 2 ;;
        --prefix) [ $# -ge 2 ] || err "--prefix needs a value"; prefix=$2; shift 2 ;;
        --archive) [ $# -ge 2 ] || err "--archive needs a value"; archive=$2; shift 2 ;;
        --no-modify-path) modify_path=0; shift ;;
        --default) make_default=1; shift ;;
        --force) force=1; shift ;;
        -h|--help)
            say "usage: get-velt.sh [--version <v>] [--prefix <dir>] [--default] [--force] [--archive <file>] [--no-modify-path]"
            exit 0 ;;
        *) err "unknown option: $1 (see --help)" ;;
    esac
done

# --- the target triple of this machine -------------------------------------------------------
detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$arch" in
        x86_64|amd64) arch=x86_64 ;;
        aarch64|arm64) arch=aarch64 ;;
        *) err "no prebuilt Velt for $os $arch; build it from source (README.md)" ;;
    esac
    case "$os" in
        Linux)
            if ls /lib/ld-musl-* >/dev/null 2>&1 || (ldd --version 2>&1 | grep -qi musl); then
                err "no prebuilt Velt for musl (Alpine); build it from source (docs/tooling/platforms.md)"
            fi
            echo "$arch-unknown-linux-gnu" ;;
        Darwin)
            # A shell running under Rosetta 2 reports x86_64 on Apple silicon.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
                arch=aarch64
            fi
            echo "$arch-apple-darwin" ;;
        MINGW*|MSYS*|CYGWIN*)
            err "on Windows, run in PowerShell: irm $base_url/releases/latest/download/get-velt.ps1 | iex" ;;
        *) err "no prebuilt Velt for $os; build it from source (README.md)" ;;
    esac
}

# --- downloads ----------------------------------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
    fetch() { curl --proto '=https,http' -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q --tries=3 -O "$2" "$1"; }
else
    fetch() { err "neither curl nor wget is installed"; }
fi

# The tag of the latest release, from the redirect of <base>/releases/latest to .../tag/<tag>.
latest_tag() {
    if command -v curl >/dev/null 2>&1; then
        url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$base_url/releases/latest") || return 1
    else
        url=$(wget -q -S --max-redirect=0 --spider "$base_url/releases/latest" 2>&1 |
            sed -n 's/^ *[Ll]ocation: *//p' | tr -d '\r' | tail -n 1)
    fi
    case "$url" in
        */tag/*) echo "${url##*/tag/}" ;;
        *) return 1 ;;
    esac
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    else
        return 1
    fi
}

# SHA256SUMS against SHA256SUMS.sig with the release key: the checksum shows the download is
# intact, the signature that the velt project published it (whatever mirror served it).
check_signature() { # <release url>
    if ! command -v openssl >/dev/null 2>&1 ||
        ! printf '%s\n' "$release_key" | openssl pkey -pubin -noout >/dev/null 2>&1 ||
        ! openssl pkeyutl -help 2>&1 | grep -q -- -rawin; then
        say "note: no OpenSSL that checks Ed25519 signatures here, so only the checksum was checked" \
            "(it shows the archive is intact, not who published it)" >&2
        return 0
    fi
    if [ -n "${VELT_INSTALL_PUBLIC_KEY:-}" ]; then
        # An Ed25519 public key's DER is a fixed 12-byte header and the 32-byte key.
        der=$(printf '302a300506032b6570032100%s' "$VELT_INSTALL_PUBLIC_KEY" | tr 'A-F' 'a-f' | awk '{
            h = "0123456789abcdef"
            for (i = 1; i < length($0); i += 2)
                printf "\\%03o", (index(h, substr($0, i, 1)) - 1) * 16 + index(h, substr($0, i + 1, 1)) - 1
        }')
        # shellcheck disable=SC2059
        b64=$(printf "$der" | openssl base64 -A) || err "VELT_INSTALL_PUBLIC_KEY is not a key"
        release_key=$(printf -- '-----BEGIN PUBLIC KEY-----\n%s\n-----END PUBLIC KEY-----' "$b64")
        printf '%s\n' "$release_key" | openssl pkey -pubin -noout >/dev/null 2>&1 ||
            err "VELT_INSTALL_PUBLIC_KEY is not an Ed25519 public key (64 hex digits)"
    fi
    fetch "$1/SHA256SUMS.sig" "$tmp/SHA256SUMS.sig" ||
        err "download failed: $1/SHA256SUMS.sig (velt 0.1.0 was published unsigned; this installer installs later releases)"
    printf '%s\n' "$release_key" > "$tmp/release-key.pem"
    # The release workflow writes the signature as its 64 raw bytes.
    sig="$tmp/SHA256SUMS.sig"
    openssl pkeyutl -verify -rawin -pubin -inkey "$tmp/release-key.pem" \
        -in "$tmp/SHA256SUMS" -sigfile "$sig" >/dev/null 2>&1 ||
        err "SHA256SUMS does not match its signature with the velt release key: refusing this download"
    say "checked the signature of SHA256SUMS with the velt release key"
}

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t velt)
trap 'rm -rf "$tmp"' EXIT INT TERM

if [ -n "$archive" ]; then
    [ -f "$archive" ] || err "no such archive: $archive"
    say "installing Velt from $archive"
else
    target=$(detect_target)
    if [ -z "$version" ] && [ "${published_version#@}" = "$published_version" ]; then
        version=$published_version
    fi
    if [ -z "$version" ]; then
        tag=$(latest_tag) || err "could not determine the latest release of $base_url (pass --version)"
        version=${tag#v}
    fi
    version=${version#v}
    name="velt-$version-$target"
    url="$base_url/releases/download/v$version"
    say "downloading Velt $version for $target"
    fetch "$url/$name.tar.gz" "$tmp/$name.tar.gz" ||
        err "download failed: $url/$name.tar.gz (is $version a release with a $target build?)"
    fetch "$url/SHA256SUMS" "$tmp/SHA256SUMS" || err "download failed: $url/SHA256SUMS"
    expected=$(awk -v f="$name.tar.gz" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
    [ -n "$expected" ] || err "SHA256SUMS has no entry for $name.tar.gz"
    if actual=$(sha256_of "$tmp/$name.tar.gz"); then
        [ "$actual" = "$expected" ] || err "checksum mismatch for $name.tar.gz (expected $expected, got $actual)"
    else
        say "warning: neither sha256sum nor shasum is installed; skipping the checksum check" >&2
    fi
    check_signature "$url"
    archive="$tmp/$name.tar.gz"
fi

# --- unpack and install -------------------------------------------------------------------------
mkdir -p "$tmp/unpacked"
tar -xzf "$archive" -C "$tmp/unpacked" || err "could not unpack $archive"
dist=
for d in "$tmp/unpacked"/*/; do
    if [ -f "$d/bin/velt" ]; then dist=${d%/}; break; fi
done
[ -n "$dist" ] || err "$archive is not a Velt release archive (no */bin/velt inside)"
[ -f "$dist/bin/velt-launcher" ] ||
    err "$archive has no launcher (bin/velt-launcher): velt 0.1.0 predates it; this installer installs later releases"
chmod +x "$dist/bin/velt" "$dist/bin/velt-launcher"
# `velt <version> (<commit> <triple>)`
installed=$("$dist/bin/velt" --version) || err "the velt in $archive does not run on this system"
version=$(printf '%s\n' "$installed" | awk '{ print $2 }')
[ -n "$version" ] || err "cannot read the version of the velt in $archive"

mkdir -p "$prefix/toolchains" "$prefix/bin" || err "cannot create $prefix"
root=$(cd "$prefix" && pwd)
dest="$root/toolchains/$version"
if [ -f "$dest/bin/velt" ] && [ "$force" = 0 ]; then
    say "velt $version is already installed in $dest (--force reinstalls it)"
else
    # Into place whole: a staging copy, renamed over (an older copy is renamed aside first).
    staging="$root/toolchains/.$version.$$"
    rm -rf "$staging"
    cp -R "$dist" "$staging"
    if [ -d "$dest" ]; then
        mv "$dest" "$root/toolchains/.$version.old.$$" ||
            err "cannot replace $dest (in use?)"
        rm -rf "$root/toolchains/.$version.old.$$"
    fi
    mv "$staging" "$dest"
    say "installed $installed into $dest"
fi

# The launcher: replaced unless the one installed belongs to a newer velt.
launcher_version() { "$1" toolchain --version 2>/dev/null | awk '{ print $2 }'; }
newer_or_same() { # <a> <b>: a >= b, by version order
    [ "$(printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n | tail -n 1)" = "$1" ]
}
current=
if [ -x "$root/bin/velt" ]; then current=$(launcher_version "$root/bin/velt" || true); fi
if [ -z "$current" ] || newer_or_same "$version" "$current"; then
    cp "$dist/bin/velt-launcher" "$root/bin/.velt.$$"
    chmod +x "$root/bin/.velt.$$"
    mv -f "$root/bin/.velt.$$" "$root/bin/velt"
fi

if [ "$make_default" = 1 ] || [ ! -s "$root/default" ]; then
    printf '%s\n' "$version" > "$root/default"
    say "velt $version is the default (packages that pin another version run that one)"
fi
bin="$root/bin"
VELT_TOOLCHAIN="$version" "$bin/velt" --version >/dev/null ||
    err "the launcher in $bin does not run velt $version"

# --- PATH ---------------------------------------------------------------------------------------
add_line() { # <file> <line>
    if [ -f "$1" ] && grep -qsF "$bin" "$1"; then return 0; fi
    mkdir -p "$(dirname "$1")"
    printf '\n# Velt\n%s\n' "$2" >> "$1"
    say "  added $bin to PATH in $1"
}
on_path=0
case ":$PATH:" in *":$bin:"*) on_path=1 ;; esac
if [ "$modify_path" = 1 ]; then
    line="export PATH=\"$bin:\$PATH\""
    add_line "$HOME/.profile" "$line"
    for rc in "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.zshrc"; do
        if [ -f "$rc" ]; then add_line "$rc" "$line"; fi
    done
    case "${SHELL:-}" in
        */zsh) add_line "$HOME/.zshrc" "$line" ;;
    esac
    if [ -d "$HOME/.config/fish" ]; then
        add_line "$HOME/.config/fish/conf.d/velt.fish" "fish_add_path -g \"$bin\""
    fi
fi

# --- next steps ---------------------------------------------------------------------------------
if ! command -v cc >/dev/null 2>&1; then
    say ""
    say "Velt links programs with the system C toolchain, which is missing:"
    case "$(uname -s)" in
        Darwin) say "  xcode-select --install" ;;
        *) say "  sudo apt install build-essential    (or: sudo dnf install gcc)" ;;
    esac
fi
say ""
if [ "$on_path" = 0 ]; then
    if [ "$modify_path" = 1 ]; then
        say "Open a new terminal, or run this in the current one:"
    else
        say "Add Velt to your PATH:"
    fi
    say "  export PATH=\"$bin:\$PATH\""
    say ""
fi
say "Then check the installation with:  velt doctor"
if [ -d "$HOME/.velt/toolchain/bin" ] && [ "$root" = "$HOME/.velt" ]; then
    say ""
    say "An earlier install (one toolchain, before the launcher) is in ~/.velt/toolchain; remove it"
    say "with \`rm -rf ~/.velt/toolchain\` and its PATH line from your shell profile."
fi
