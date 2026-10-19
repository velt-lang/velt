#!/usr/bin/env sh
# Install a Velt dist directory (from scripts/package.sh, or an unpacked release .tar.gz) beside
# the other installed versions (#948, docs/tooling/platforms.md): as <root>/toolchains/<version>,
# with its launcher as <root>/bin/velt. Replaces an installed copy of the same version (a rebuild).
# Does not modify PATH; prints the command to do so.
#
# Usage: scripts/install.sh <dist dir> [<root>]     (root default: ~/.velt)
#
# To use a build without installing it, link it instead: velt toolchain link dev <dist dir>.
set -eu

if [ $# -lt 1 ]; then
    echo "usage: $0 <dist dir> [<root>]" >&2
    exit 2
fi
dist=$(cd "$1" && pwd)
root=${2:-"$HOME/.velt"}

for f in bin/velt bin/velt-launcher lib/libvelt_rt.a; do
    [ -f "$dist/$f" ] || { echo "error: $dist is not a Velt dist directory (missing $f)" >&2; exit 1; }
done
# `velt <version> (<commit> <triple>)`
version=$("$dist/bin/velt" --version | awk '{ print $2 }')
[ -n "$version" ] || { echo "error: cannot read the version of $dist/bin/velt" >&2; exit 1; }

mkdir -p "$root/toolchains" "$root/bin"
root=$(cd "$root" && pwd)
dest="$root/toolchains/$version"
staging="$root/toolchains/.$version.$$"
# A failed install leaves no partial toolchain behind.
trap 'if [ -n "$staging" ]; then rm -rf "$staging"; fi' EXIT INT TERM
rm -rf "$staging"
cp -R "$dist" "$staging"
if [ -d "$dest" ]; then
    mv "$dest" "$root/toolchains/.$version.old.$$"
    rm -rf "$root/toolchains/.$version.old.$$"
fi
mv "$staging" "$dest"
staging=
cp "$dist/bin/velt-launcher" "$root/bin/.velt.$$"
chmod +x "$root/bin/.velt.$$"
mv -f "$root/bin/.velt.$$" "$root/bin/velt"
if [ ! -s "$root/default" ]; then
    printf '%s\n' "$version" > "$root/default"
fi

echo "installed velt $version into $dest (default: $(cat "$root/default"))"
echo
echo "Add the launcher to your PATH (append this line to ~/.profile, ~/.bashrc or ~/.zshrc):"
echo "  export PATH=\"$root/bin:\$PATH\""
echo
echo "Then check the installation with:  velt doctor"
