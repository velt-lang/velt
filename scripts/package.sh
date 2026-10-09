#!/usr/bin/env sh
# Build a release Velt toolchain and assemble dist/velt-<version>-<host triple>/ (+ .tar.gz).
#
# Layout (see docs/tooling/platforms.md):
#   bin/velt  lib/libvelt_rt.a  lib/libvelt_rt_shared.{so,dylib}  lib/NATIVE_LIBS.md
#   lib/velt/lld  lib/targets/<host>/ (the link kit)
# and the target packs dist/velt-<version>-target-<triple>.tar.gz (`velt target add`): this host's
# runtime and kit, and on Linux <arch>-unknown-linux-musl's.
#   std/**  README.md  LICENSE-MIT  LICENSE-APACHE  NOTICE
#
# Usage: scripts/package.sh [--std-dir <dir>] [--skip-build] [--no-archive] [--lld <path>]
#                           [--no-bundled-linker] [--no-musl]
#   --std-dir            std sources to ship (default: <repo>/std)
#   --skip-build         reuse the existing release build
#   --no-archive         only assemble the directory
#   --lld                the lld to bundle (scripts/build-lld.sh builds one; default: build it
#                        once into $VELT_LLD_CACHE, ~/.cache/velt/lld-<LLVM version>)
#   --no-bundled-linker  ship no lld and no link kits (programs link with the system linker)
#   --no-musl            (Linux) skip the musl target pack
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
std_dir="$repo/std"
build=1
archive=1
lld=${VELT_LLD:-}
bundled=1
musl=1
while [ $# -gt 0 ]; do
    case "$1" in
        --std-dir) std_dir=$2; shift 2 ;;
        --skip-build) build=0; shift ;;
        --no-archive) archive=0; shift ;;
        --lld) lld=$2; shift 2 ;;
        --no-bundled-linker) bundled=0; shift ;;
        --no-musl) musl=0; shift ;;
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

case "$host" in *linux-gnu*) ;; *) musl=0 ;; esac
musl_target="${host%%-*}-unknown-linux-musl"

if [ "$build" = 1 ]; then
    echo "building release velt + velt_rt + velt_rt_shared + velt-kit..."
    cargo build --release -p veltc -p velt_rt -p velt_rt_shared -p velt_link --manifest-path "$repo/Cargo.toml"
    if [ "$bundled" = 1 ] && [ "$musl" = 1 ]; then
        echo "building the runtime for $musl_target..."
        rustup target add "$musl_target"
        # cc-rs looks for <arch>-linux-musl-gcc; Debian's musl-tools installs musl-gcc.
        cc_var="CC_$(echo "$musl_target" | tr - _)"
        if ! command -v "${host%%-*}-linux-musl-gcc" >/dev/null 2>&1 && command -v musl-gcc >/dev/null 2>&1; then
            export "$cc_var=musl-gcc"
        fi
        cargo build --release -p velt_rt --target "$musl_target" --manifest-path "$repo/Cargo.toml"
    fi
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

cp "$repo/LICENSE-MIT" "$repo/LICENSE-APACHE" "$repo/NOTICE" "$out/"

# The bundled linker (crates/velt_link/src/bundled.rs): lld plus a link kit per target.
if [ "$bundled" = 1 ]; then
    before=$(du -sk "$out" | cut -f1)
    if [ -z "$lld" ]; then
        llvm_version=$(sed -n 's/^VERSION=${LLVM_VERSION:-\(.*\)}$/\1/p' "$repo/scripts/build-lld.sh")
        cache=${VELT_LLD_CACHE:-"$HOME/.cache/velt/lld-$llvm_version"}
        [ -x "$cache/lld" ] || "$repo/scripts/build-lld.sh" "$cache"
        lld="$cache/lld"
    fi
    mkdir -p "$out/lib/velt"
    cp "$lld" "$out/lib/velt/lld"
    cp "$repo/crates/velt_link/kit/licenses/LLVM-LICENSE.txt" "$out/lib/velt/LICENSE.txt"
    "$release/velt-kit" build --target "$host" --lld "$out/lib/velt/lld" --out "$out/lib/targets/$host"
    after=$(du -sk "$out" | cut -f1)
    echo "bundled linker: lld $(du -sk "$out/lib/velt" | cut -f1) KiB, kit $(du -sk "$out/lib/targets" | cut -f1) KiB; toolchain $before -> $after KiB"

    # Target packs (`velt target add`, docs/tooling/platforms.md): a target's runtime and link
    # kit, for toolchains on other hosts (this host's) and for musl (Linux).
    packs="$dist/packs"
    rm -rf "$packs"
    mkdir -p "$packs"
    cp -R "$out/lib/targets/$host" "$packs/$host"
    cp "$release/libvelt_rt.a" "$packs/$host/"
    if [ "$musl" = 1 ]; then
        "$release/velt-kit" build --target "$musl_target" --lld "$out/lib/velt/lld" \
            --runtime "$target_dir/$musl_target/release/libvelt_rt.a" --out "$packs/$musl_target"
        cp "$repo/crates/velt_link/kit/licenses/musl-COPYRIGHT.txt" "$packs/$musl_target/COPYRIGHT"
        # libunwind.a, crtbegin.o and crtend.o are LLVM's.
        cp "$repo/crates/velt_link/kit/licenses/LLVM-LICENSE.txt" "$packs/$musl_target/LLVM-LICENSE.txt"
    fi
    for pack in "$packs"/*; do
        triple=$(basename "$pack")
        # The runtime bundles third-party crates: their notices and Velt's licenses go along.
        cp "$repo/NOTICE" "$repo/LICENSE-MIT" "$repo/LICENSE-APACHE" "$pack/"
        file="velt-$version-target-$triple.tar.gz"
        # No macOS metadata (`._*` files) in the archive.
        COPYFILE_DISABLE=1 tar -czf "$dist/$file" -C "$packs" "$triple"
        echo "target pack: $dist/$file ($(du -sk "$dist/$file" | cut -f1) KiB)"
        # The toolchain lists its packs' hashes (`velt target add` checks packs against them);
        # the release workflow replaces the list with every target's.
        (cd "$dist" && { command -v sha256sum >/dev/null && sha256sum "$file" || shasum -a 256 "$file"; }) \
            >> "$out/lib/targets/PACKS.sha256"
    done
    rm -rf "$packs"
fi

if [ "$archive" = 1 ]; then
    COPYFILE_DISABLE=1 tar -czf "$dist/$name.tar.gz" -C "$dist" "$name"
    echo "archive: $dist/$name.tar.gz"
fi
echo "dist:    $out"
