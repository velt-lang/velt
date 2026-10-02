#!/usr/bin/env sh
# Build a release Velt toolchain and assemble dist/velt-<version>-<host triple>/ (+ .tar.gz).
#
# Layout (see docs/tooling/platforms.md):
#   bin/velt  lib/libvelt_rt.a  lib/libvelt_rt_shared.{so,dylib}  lib/NATIVE_LIBS.md  std/**  README.md  LICENSE-MIT  LICENSE-APACHE
#
# Usage: scripts/package.sh [--std-dir <dir>] [--skip-build] [--no-archive]
#   --std-dir     std sources to ship (default: <repo>/std)
#   --skip-build  reuse the existing release build
#   --no-archive  only assemble the directory
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
std_dir="$repo/std"
build=1
archive=1
while [ $# -gt 0 ]; do
    case "$1" in
        --std-dir) std_dir=$2; shift 2 ;;
        --skip-build) build=0; shift ;;
        --no-archive) archive=0; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
target_dir=${CARGO_TARGET_DIR:-"$repo/target"}

version=$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$repo/Cargo.toml" | head -n 1)
host=$(rustc -vV | sed -n 's/^host: *//p')
if [ -z "$version" ] || [ -z "$host" ]; then
    echo "error: could not determine the version or host triple" >&2
    exit 1
fi

if [ "$build" = 1 ]; then
    echo "building release velt + velt_rt + velt_rt_shared..."
    cargo build --release -p veltc -p velt_rt -p velt_rt_shared --manifest-path "$repo/Cargo.toml"
fi

release="$target_dir/release"
# The shared runtime debug builds link (crates/velt_rt_shared).
case "$host" in
    *apple*) shared_rt="libvelt_rt_shared.dylib" ;;
    *) shared_rt="libvelt_rt_shared.so" ;;
esac
for f in "$release/velt" "$release/libvelt_rt.a" "$release/$shared_rt"; do
    [ -f "$f" ] || { echo "error: missing build output: $f" >&2; exit 1; }
done

name="velt-$version-$host"
dist="$repo/dist"
out="$dist/$name"
rm -rf "$out"
mkdir -p "$out/bin" "$out/lib" "$out/std"

cp "$release/velt" "$out/bin/"
cp "$release/libvelt_rt.a" "$release/$shared_rt" "$out/lib/"
cp "$repo/crates/velt_rt/NATIVE_LIBS.md" "$out/lib/"
if [ -d "$std_dir" ]; then
    cp -R "$std_dir/." "$out/std/"
else
    echo "warning: no std sources at $std_dir; shipping an empty std/ (pass --std-dir)" >&2
fi

cat > "$out/README.md" <<EOF
# Velt $version ($host)

Install:  get-velt.sh --archive <this .tar.gz> (an asset of every release), or
          scripts/install.sh <this directory> from a source checkout, or copy it anywhere.
Then add \`<prefix>/bin\` to PATH and run \`velt doctor\`.

    velt run hello.vlt
    velt new app && cd app && velt run

Layout: bin/ (the velt CLI), lib/ (runtime library linked into every program),
std/ (standard library sources). Full guide: docs/tooling/platforms.md in the Velt repository.
EOF

cp "$repo/LICENSE-MIT" "$repo/LICENSE-APACHE" "$out/"

if [ "$archive" = 1 ]; then
    tar -czf "$dist/$name.tar.gz" -C "$dist" "$name"
    echo "archive: $dist/$name.tar.gz"
fi
echo "dist:    $out"
