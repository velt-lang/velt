# #3202 diagnostic (not for merge): sleeper_on_spare from std_scheduler_rescue.rs (a copy that
# prints which worker each part ran on), run directly as the test runs it (VELT_THREADS=1,
# VELT_RESCUE=1, the checking allocator and counts on), until a run shows worker 0 not lazy again
# (locks-on=1). Keeps every run's output; prints the failing one. No trace unless -Trace > 0.
#   pwsh notes-3202/spare.ps1 -Velt <target>/debug/velt.exe [-Runs 60] [-Trace 0]
param([string]$Velt, [int]$Runs = 60, [int]$Trace = 0)
$dir = Join-Path $env:TEMP "spare3202"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$src = Join-Path $dir "sleeper_on_spare.velt"
Copy-Item (Join-Path $PSScriptRoot "sleeper_on_spare.velt") $src -Force
$exe = Join-Path $dir "sleeper_on_spare.exe"
$env:VELT_SCHEDULER = "std"
& $Velt build $src -o $exe
if ($LASTEXITCODE -ne 0) { throw "build failed" }
Remove-Item Env:VELT_SCHEDULER
$env:VELT_THREADS = "1"; $env:VELT_RESCUE = "1"; $env:VELT_RESCUE_STATS = "1"
$env:VELT_RT_DEBUG_ALLOC = "1"; $env:VELT_RC_STATS = "1"
if ($Trace -gt 0) { $env:VELT_SCHED_DEBUG = "$Trace" } else { Remove-Item Env:VELT_SCHED_DEBUG -ErrorAction Ignore }
$tally = @{}
for ($i = 1; $i -le $Runs; $i++) {
  $out = Join-Path $dir "run$i.out"
  $err = Join-Path $dir "run$i.err"
  & $exe > $out 2> $err
  $stats = (Select-String -Path $err -Pattern "rescue stats:" | Select-Object -Last 1).Line
  $lines = (Get-Content $out) -join " | "
  $key = "$stats :: $lines"
  $tally[$key] = 1 + [int]$tally[$key]
  if ($stats -match "locks-on=1\b") {
    Write-Output "not lazy again in run ${i}:"
    Write-Output "  stdout: $lines"
    Write-Output "  stats:  $stats"
  }
}
Write-Output "tally of $Runs runs:"
$tally.GetEnumerator() | Sort-Object Value -Descending | ForEach-Object { Write-Output ("{0,4}  {1}" -f $_.Value, $_.Key) }
