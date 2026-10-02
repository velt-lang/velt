#!/usr/bin/env bash
# The checks your changes need, on macOS or Linux: the files changed since the merge base with
# origin/main, plus uncommitted and untracked ones, select them (rules in
# crates/xtask/src/plan.rs; `cargo xtask affected` prints the plan and why). Goldens run in debug
# mode only. The merge queue runs the whole gate (scripts/check-all.sh) on three OSes.
#   scripts/check.sh                          # what the changes need
#   scripts/check.sh --part test              # one part: lint, test or golden
#   scripts/check.sh --golden-modes release   # release-mode goldens instead
#   scripts/check.sh --full                   # everything (as check-all.sh --fast)
# Other options of `cargo xtask check` pass through (`cargo xtask help`).
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo xtask check --fast "$@"
