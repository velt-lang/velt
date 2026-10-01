#!/usr/bin/env sh
# Run scripts/check-all.sh on Linux aarch64 (Ubuntu 24.04) from an Apple Silicon Mac, inside an
# OrbStack machine. The committed HEAD of this checkout is fetched into a clone on the machine's
# own filesystem (default ~/velt-arm64; building on the shared /Users mount is slower and would
# share target/ with the host). Uncommitted changes are NOT tested.
#
# Usage: scripts/test-linux-arm64.sh [--setup] [--machine <name>] [--dir <clone dir>] [-- <check-all args>]
#   --setup   create the machine (ubuntu:noble, arm64) if missing and install build-essential,
#             clang-18, git, curl and rustup stable
# Prerequisite: OrbStack (`brew install --cask orbstack`).
set -eu

machine=velt-arm64
dir='~/velt-arm64'
setup=0
while [ $# -gt 0 ]; do
    case "$1" in
        --setup) setup=1; shift ;;
        --machine) machine=$2; shift 2 ;;
        --dir) dir=$2; shift 2 ;;
        --) shift; break ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

if ! command -v orb >/dev/null 2>&1; then
    echo "error: OrbStack's \`orb\` not found; install it with \`brew install --cask orbstack\`" >&2
    exit 1
fi

repo=$(cd "$(dirname "$0")/.." && pwd)

provision() {
    if ! orb list 2>/dev/null | grep -q "^$machine "; then
        orb create -a arm64 ubuntu:noble "$machine"
    fi
    orb -m "$machine" -u root bash -lc 'set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q build-essential clang-18 git curl ca-certificates'
    orb -m "$machine" bash -lc 'set -eu
if ! command -v rustup >/dev/null 2>&1; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal -c clippy,rustfmt
fi
. "$HOME/.cargo/env"
rustup update stable'
}

[ "$setup" = 1 ] && provision

commit=$(git -C "$repo" rev-parse HEAD)
common=$(git -C "$repo" rev-parse --path-format=absolute --git-common-dir)
if [ -n "$(git -C "$repo" status --porcelain)" ]; then
    echo "warning: uncommitted changes are not tested (the VM tests commit ${commit%"${commit#???????}"})" >&2
fi
# OrbStack mounts macOS's /Users at the same path, so the repository is fetched directly.
script="set -eu
. \"\$HOME/.cargo/env\"
dir=$dir
if [ ! -d \"\$dir/.git\" ]; then git -c safe.directory='*' clone -q --no-checkout '$common' \"\$dir\"; fi
cd \"\$dir\"
git -c safe.directory='*' fetch -q '$common' '+refs/heads/*:refs/remotes/mac/*'
git checkout -q --detach $commit
bash scripts/check-all.sh $*"
orb -m "$machine" bash -lc "$script"
