#!/usr/bin/env bash
# The differential test of C structs by value (crates/veltc/tests/c_struct_by_value.rs, #2187
# layer 3) for arm64 Linux's convention (AAPCS64), from an x86-64 Linux machine: the runtime is
# built for aarch64-unknown-linux-musl with zig as its C compiler and linker, the test's
# programs are built for that target and run under qemu (`VELT_ABI_TARGET`, `VELT_ABI_RUNNER`).
# For a machine without arm64 Linux at hand (CI runs the test natively there: ci.yml's
# c-struct-abi job). Needs VELT_ZIG (a zig 0.17.1), rustup and qemu-aarch64-static (Debian and
# Ubuntu: `apt-get install qemu-user-static`).
#
# Usage: scripts/abi-cross.sh [<cargo test arguments>...]
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
zig=${VELT_ZIG:?set VELT_ZIG to a zig 0.17.1}
runner=${VELT_ABI_RUNNER:-qemu-aarch64-static}
command -v "$runner" >/dev/null || { echo "error: \`$runner\` not found (apt-get install qemu-user-static)" >&2; exit 1; }
target=aarch64-unknown-linux-musl
env_target=${target//-/_}
work=${RUNNER_TEMP:-${TMPDIR:-/tmp}}/velt-abi-cross
mkdir -p "$work"

# zig's cc for the target; cc-rs adds a `--target=` zig doesn't read.
cat >"$work/cc" <<EOF
#!/usr/bin/env bash
args=()
for a in "\$@"; do case "\$a" in --target=*) ;; *) args+=("\$a") ;; esac; done
exec "$zig" cc -target aarch64-linux-musl "\${args[@]}"
EOF
printf '#!/bin/sh\nexec "%s" ar "$@"\n' "$zig" >"$work/ar"
chmod +x "$work/cc" "$work/ar"

rustup target add "$target"
export "CC_$env_target=$work/cc" "AR_$env_target=$work/ar"
export "CARGO_TARGET_${env_target^^}_LINKER=$work/cc"
cargo build --release -p velt_rt --target "$target" --manifest-path "$repo/Cargo.toml"

target_dir=${CARGO_TARGET_DIR:-$repo/target}
VELT_ABI_TARGET=$target VELT_ABI_RUNNER=$runner \
    VELT_RT_LIB="$target_dir/$target/release/libvelt_rt.a" VELT_LINKER="$work/cc" \
    cargo test --workspace --manifest-path "$repo/Cargo.toml" --test c_struct_by_value "$@"
