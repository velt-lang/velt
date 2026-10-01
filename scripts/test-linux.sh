#!/usr/bin/env sh
# Build and test Velt on Linux: cargo build + cargo test --workspace (goldens in debug and
# release; release uses LLVM when clang 16+ is installed), `velt doctor`, `velt new` + `run`, and
# an HTTP smoke test of examples/http_hello.vlt.
#
# - From Windows (Git Bash): runs inside WSL. The committed HEAD of this checkout is fetched into
#   a clone on the WSL filesystem (default ~/velt-linux; building on /mnt/c is very slow) and the
#   script re-runs itself there. Uncommitted changes are NOT tested.
# - On Linux: runs in this checkout.
#
# Usage: scripts/test-linux.sh [--distro <wsl distro>] [--dir <clone dir in WSL>] [--quick]
#   --quick   skip `cargo test` (build, doctor, new/run and HTTP smoke only)
# WSL prerequisites: build-essential, git, curl, rustup stable; optional clang 16+ (e.g. clang-18).
set -eu

distro=Ubuntu
dir='~/velt-linux'
quick=0
while [ $# -gt 0 ]; do
    case "$1" in
        --distro) distro=$2; shift 2 ;;
        --dir) dir=$2; shift 2 ;;
        --quick) quick=1; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

repo=$(cd "$(dirname "$0")/.." && pwd)

run_from_windows() {
    commit=$(git -C "$repo" rev-parse HEAD)
    common=$(git -C "$repo" rev-parse --path-format=absolute --git-common-dir)
    if [ -n "$(git -C "$repo" status --porcelain)" ]; then
        echo "warning: uncommitted changes are not tested (WSL tests commit ${commit%"${commit#???????}"})" >&2
    fi
    flags=""
    [ "$quick" = 1 ] && flags="--quick"
    # Fetch every branch of the Windows repository, then check out exactly this commit.
    script="set -eu
src=\$(wslpath -u '$common')
dir=$dir
if [ ! -d \"\$dir/.git\" ]; then git -c safe.directory='*' clone -q --no-checkout \"\$src\" \"\$dir\"; fi
cd \"\$dir\"
git -c safe.directory='*' fetch -q \"\$src\" '+refs/heads/*:refs/remotes/win/*'
git checkout -q --detach $commit
sh scripts/test-linux.sh $flags"
    # `--exec` hands the script to bash verbatim (`--` would re-parse it with the WSL shell).
    MSYS_NO_PATHCONV=1 wsl.exe -d "$distro" --exec bash -lc "$script"
}

step() {
    printf '\n== %s\n' "$*"
}

run_on_linux() {
    cd "$repo"
    export CARGO_INCREMENTAL=0
    step "cargo build --workspace"
    cargo build --workspace
    if [ "$quick" = 0 ]; then
        step "cargo test --workspace"
        cargo test --workspace
    fi
    sh "$repo/scripts/smoke.sh" "$repo/target/debug/velt"
    step "all Linux checks passed ($(git rev-parse --short HEAD 2>/dev/null || echo unknown))"
}

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) run_from_windows ;;
    *) run_on_linux ;;
esac
