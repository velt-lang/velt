# Runs every quality gate: build, clippy, unit tests, strict
# goldens (debug + release), formatting of .vlt sources, and optionally the Linux run in WSL.
#   pwsh scripts/check-all.ps1            # Windows gates
#   pwsh scripts/check-all.ps1 -Linux     # + scripts/test-linux.sh via WSL
#   pwsh scripts/check-all.ps1 -Fast      # while iterating: goldens in debug mode only (the
#                                         # full gate runs before merging)
param([switch]$Linux, [switch]$Fast)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
$env:CARGO_INCREMENTAL = "0"
if ($Fast) { $env:VELT_GOLDEN_MODES = "debug" }

function Step($name, [scriptblock]$body) {
  Write-Host "==> $name" -ForegroundColor Cyan
  $sw = [Diagnostics.Stopwatch]::StartNew()
  & $body
  if ($LASTEXITCODE -ne 0) { Write-Host "FAILED: $name" -ForegroundColor Red; exit 1 }
  Write-Host ("    ok ({0:N0}s)" -f $sw.Elapsed.TotalSeconds) -ForegroundColor Green
}

Step "build" { cargo build --workspace --all-targets }
Step "clippy" { cargo clippy --workspace --all-targets -- -D warnings }
# The end-to-end goldens are their own step below (run once, with their summary); --no-fail-fast
# reports every failing test binary in one run.
Step "unit + integration tests" { cargo test --workspace --no-fail-fast -- --skip golden --exact }
# Non-strict: goldens under a `.pending` dir (known bugs, milestones in progress) are reported only.
Step "goldens (debug + release)" { cargo test -p veltc --test golden -- --nocapture }
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
Step "velt fmt --check (std, examples)" { & (Join-Path $TargetDir "debug/velt") fmt --check std examples }
if ($Linux) {
  Step "linux (WSL)" { bash scripts/test-linux.sh }
}
Write-Host "all gates passed" -ForegroundColor Green
