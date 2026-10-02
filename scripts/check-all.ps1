# The whole quality gate on Windows (what the merge queue runs on each OS): fmt, build, clippy,
# unit and integration tests, goldens (debug + release), `velt fmt --check` of std and examples,
# and optionally the Linux run in WSL. The steps are in crates/xtask/src/check.rs;
# scripts/check.ps1 runs only the ones your changes need.
#   pwsh scripts/check-all.ps1            # Windows gates
#   pwsh scripts/check-all.ps1 -Linux     # + scripts/test-linux.sh via WSL
#   pwsh scripts/check-all.ps1 -Fast      # goldens in debug mode only
param([switch]$Linux, [switch]$Fast)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
$xtaskArgs = @("xtask", "check", "--full")
if ($Fast) { $xtaskArgs += "--fast" }
cargo @xtaskArgs
if ($LASTEXITCODE -ne 0) { exit 1 }
if ($Linux) {
  Write-Host "==> linux (WSL)" -ForegroundColor Cyan
  bash scripts/test-linux.sh
  if ($LASTEXITCODE -ne 0) { Write-Host "FAILED: linux (WSL)" -ForegroundColor Red; exit 1 }
}
Write-Host "all gates passed" -ForegroundColor Green
