#!/usr/bin/env bash
# The whole quality gate on macOS or Linux (what the merge queue runs on each OS): fmt, build,
# clippy, unit and integration tests, goldens (debug + release), `velt fmt --check` of std and
# examples, then scripts/smoke.sh (doctor, new/run, HTTP example). The steps are in
# crates/xtask/src/check.rs; scripts/check.sh runs only the ones your changes need.
#   scripts/check-all.sh              # all gates
#   scripts/check-all.sh --no-smoke   # skip the smoke test
#   scripts/check-all.sh --fast       # goldens in debug mode only, no smoke test
# Other options of `cargo xtask check` pass through (`cargo xtask help`).
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo xtask check --full "$@"
