#!/usr/bin/env sh
# Download and install a released Velt toolchain on Linux or macOS.
#
#   curl -fsSL https://github.com/velt-lang/velt/releases/latest/download/get-velt.sh | sh
#   curl -fsSL https://github.com/velt-lang/velt/releases/latest/download/get-velt.sh | sh -s -- --prefix ~/velt
#
# Options (environment variable in parentheses):
#   --version <v>     the release to install, e.g. 0.1.0 (VELT_INSTALL_VERSION); default: the release
#                     this script was published with, or the latest release
#   --prefix <dir>    where to install (VELT_INSTALL_PREFIX); default: ~/.velt/toolchain
#   --archive <file>  install a downloaded velt-<version>-<target>.tar.gz instead of downloading
#   --no-modify-path  do not add <prefix>/bin to PATH in your shell profiles
# VELT_INSTALL_BASE_URL replaces https://github.com/velt-lang/velt (forks, mirrors, tests).
#
# The prefix's bin/, lib/ and std/ are replaced wholesale (docs/tooling/platforms.md).
set -eu

# The release workflow replaces this with the version it publishes.
published_version="@VELT_RELEASE_VERSION@"

base_url=${VELT_INSTALL_BASE_URL:-https://github.com/velt-lang/velt}
version=${VELT_INSTALL_VERSION:-}
prefix=${VELT_INSTALL_PREFIX:-"$HOME/.velt/toolchain"}
archive=
modify_path=1

say() { printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || err "--version needs a value"; version=$2; shift 2 ;;
        --prefix) [ $# -ge 2 ] || err "--prefix needs a value"; prefix=$2; shift 2 ;;
        --archive) [ $# -ge 2 ] || err "--archive needs a value"; archive=$2; shift 2 ;;
        --no-modify-path) modify_path=0; shift ;;
        -h|--help)
            say "usage: get-velt.sh [--version <v>] [--prefix <dir>] [--archive <file>] [--no-modify-path]"
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

mkdir -p "$prefix" || err "cannot create $prefix"
prefix=$(cd "$prefix" && pwd)
for d in bin lib std; do
    rm -rf "${prefix:?}/$d"
    if [ -d "$dist/$d" ]; then cp -R "$dist/$d" "$prefix/$d"; fi
done
for f in README.md LICENSE-MIT LICENSE-APACHE; do
    if [ -f "$dist/$f" ]; then cp "$dist/$f" "$prefix/"; fi
done
chmod +x "$prefix/bin/velt"
bin="$prefix/bin"
installed=$("$bin/velt" --version) || err "the installed $bin/velt does not run on this system"
say "installed $installed into $prefix"

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
