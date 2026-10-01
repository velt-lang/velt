#!/usr/bin/env bash
# Runs every quality gate, on macOS or Linux (the bash
# counterpart of scripts/check-all.ps1): build, clippy, unit tests, goldens (debug + release),
# formatting of .vlt sources, then scripts/smoke.sh (doctor, new/run, HTTP example).
#   scripts/check-all.sh              # all gates
#   scripts/check-all.sh --no-smoke   # skip the smoke test
#   scripts/check-all.sh --fast       # while iterating: goldens in debug mode only, no smoke test
#                                     # (the full gate runs before merging)
set -euo pipefail

smoke=1
while [ $# -gt 0 ]; do
    case "$1" in
        --no-smoke) smoke=0; shift ;;
        --fast) smoke=0; export VELT_GOLDEN_MODES=debug; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

cd "$(dirname "$0")/.."
export CARGO_INCREMENTAL=0

step() {
    local name=$1
    shift
    printf '\033[36m==> %s\033[0m\n' "$name"
    local start=$SECONDS
    if ! "$@"; then
        printf '\033[31mFAILED: %s\033[0m\n' "$name"
        exit 1
    fi
    printf '\033[32m    ok (%ds)\033[0m\n' $((SECONDS - start))
}

# musl targets link statically and cannot build a cdylib, so the shared runtime (an optional
# speed-up for debug links; `velt` falls back to the static runtime) is left out there.
workspace=(--workspace)
if rustc -vV | grep -q '^host: .*-musl'; then
    workspace+=(--exclude velt_rt_shared)
fi

step "build" cargo build "${workspace[@]}" --all-targets
step "clippy" cargo clippy "${workspace[@]}" --all-targets -- -D warnings
step "cargo fmt --check" cargo fmt --all --check
# The end-to-end goldens are their own step below (run once, with their summary); --no-fail-fast
# reports every failing test binary in one run.
step "unit + integration tests" cargo test "${workspace[@]}" --no-fail-fast -- --skip golden --exact
# Non-strict: goldens under a `.pending` dir (known bugs, milestones in progress) are reported only.
step "goldens (debug + release)" cargo test -p veltc --test golden -- --nocapture
velt_bin="${CARGO_TARGET_DIR:-target}/debug/velt"
step "velt fmt --check (std, examples)" "$velt_bin" fmt --check std examples
if [ "$smoke" = 1 ]; then
    step "smoke (doctor, new/run, HTTP)" sh scripts/smoke.sh "$velt_bin"
fi
printf '\033[32mall gates passed (%s, %s)\033[0m\n' "$(uname -sm)" \
    "$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
