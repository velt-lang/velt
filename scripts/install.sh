#!/usr/bin/env sh
# Install a Velt dist directory (from scripts/package.sh, or an unpacked release .tar.gz) into a
# prefix. Does not modify PATH; prints the command to do so.
#
# Usage: scripts/install.sh <dist dir> [<prefix>]     (prefix default: ~/.velt/toolchain)
set -eu

if [ $# -lt 1 ]; then
    echo "usage: $0 <dist dir> [<prefix>]" >&2
    exit 2
fi
dist=$(cd "$1" && pwd)
prefix=${2:-"$HOME/.velt/toolchain"}

for f in bin/velt lib/libvelt_rt.a; do
    [ -f "$dist/$f" ] || { echo "error: $dist is not a Velt dist directory (missing $f)" >&2; exit 1; }
done

mkdir -p "$prefix"
prefix=$(cd "$prefix" && pwd)
# Replace the toolchain parts wholesale so files removed upstream (e.g. std modules) disappear.
for d in bin lib std share/velt; do
    rm -rf "${prefix:?}/$d"
    if [ -d "$dist/$d" ]; then mkdir -p "$(dirname "$prefix/$d")" && cp -R "$dist/$d" "$prefix/$d"; fi
done
for f in README.md LICENSE-MIT LICENSE-APACHE NOTICE; do
    if [ -f "$dist/$f" ]; then cp "$dist/$f" "$prefix/"; fi
done
chmod +x "$prefix/bin/velt"

echo "installed Velt into $prefix"
echo
echo "Add it to your PATH (append this line to ~/.profile, ~/.bashrc or ~/.zshrc):"
echo "  export PATH=\"$prefix/bin:\$PATH\""
echo
echo "Then check the installation with:  velt doctor"
