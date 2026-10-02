# The checks your changes need, on Windows: the files changed since the merge base with
# origin/main, plus uncommitted and untracked ones, select them (rules in
# crates/xtask/src/plan.rs; `cargo xtask affected` prints the plan and why). Goldens run in debug
# mode only. The merge queue runs the whole gate (scripts/check-all.ps1) on three OSes.
#   pwsh scripts/check.ps1                          # what the changes need
#   pwsh scripts/check.ps1 --part test              # one part: lint, test or golden
#   pwsh scripts/check.ps1 --golden-modes release   # release-mode goldens instead
#   pwsh scripts/check.ps1 --full                   # everything (as check-all.ps1 -Fast)
# Other options of `cargo xtask check` pass through (`cargo xtask help`).
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
cargo xtask check --fast @args
exit $LASTEXITCODE
