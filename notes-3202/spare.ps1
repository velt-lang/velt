# #3202 diagnostic (not for merge): sleeper_on_spare from std_scheduler_rescue.rs, run directly
# with the scheduler trace, until a run shows worker 0 not lazy again (locks-on=1). Keeps that
# run's stderr. Run from the worktree root with the built velt:
#   pwsh notes-3202/spare.ps1 -Velt <target>/debug/velt.exe [-Runs 40] [-Trace 25]
param([string]$Velt, [int]$Runs = 40, [int]$Trace = 25)
$dir = Join-Path $env:TEMP "spare3202"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$src = Join-Path $dir "sleeper_on_spare.velt"
Copy-Item (Join-Path $PSScriptRoot "sleeper_on_spare.velt") $src
$exe = Join-Path $dir "sleeper_on_spare.exe"
$env:VELT_SCHEDULER = "std"
& $Velt build $src -o $exe
if ($LASTEXITCODE -ne 0) { throw "build failed" }
Remove-Item Env:VELT_SCHEDULER
$env:VELT_THREADS = "1"; $env:VELT_RESCUE = "1"; $env:VELT_RESCUE_STATS = "1"
$env:VELT_SCHED_DEBUG = "$Trace"
for ($i = 1; $i -le $Runs; $i++) {
  $err = Join-Path $dir "run$i.err"
  & $exe 2> $err | Out-Null
  $stats = Select-String -Path $err -Pattern "rescue stats:" | Select-Object -Last 1
  Write-Output "run ${i}: exit $LASTEXITCODE $($stats.Line)"
  if ($stats -and $stats.Line -match "locks-on=1\b") {
    Write-Output "not lazy again in run ${i}: trace kept in $err"
    break
  }
}
