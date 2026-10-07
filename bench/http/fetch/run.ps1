# The fetch client benchmark on Windows (bench/http/fetch/README.md): Velt's global `fetch`
# against Node's (undici) and Rust's reqwest, all calling the same local hyper server.
# Usage: pwsh bench/http/fetch/run.ps1 [-Scenarios seq,conc,big,json] [-Runs 3]
#        [-Clients velt,node,reqwest] [-Velt <path to a release velt>]
# Prints one line per run: the client's own result, then CPU seconds (user + system) and peak
# working set, sampled while it runs. Only stops the server it started.
param(
  [string[]]$Scenarios = @("seq", "conc", "big", "json"),
  [int]$Runs = 3,
  [string[]]$Clients = @("velt", "node", "reqwest"),
  [string]$Velt = "velt"
)
$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("fetch-bench-" + [guid]::NewGuid())
New-Item -ItemType Directory $work | Out-Null
$target = if ($env:BENCH_TARGET_DIR) { $env:BENCH_TARGET_DIR } else { Join-Path $work "target" }
$server = $null
try {
  $env:CARGO_TARGET_DIR = $target
  cargo build --release --quiet --manifest-path (Join-Path $here "rust/Cargo.toml")
  & $Velt build --release (Join-Path $here "client.vlt") -o (Join-Path $work "client-velt.exe")
  $server = Start-Process (Join-Path $target "release/server.exe") -ArgumentList "18080" -PassThru -NoNewWindow
  Start-Sleep -Seconds 1
  $base = "http://127.0.0.1:18080"

  function Measure-Client([string]$exe, [string[]]$arguments) {
    $p = Start-Process $exe -ArgumentList $arguments -PassThru -NoNewWindow
    $peak = 0; $cpu = [TimeSpan]::Zero
    while (-not $p.HasExited) {
      try { $p.Refresh(); $peak = [Math]::Max($peak, $p.PeakWorkingSet64); $cpu = $p.TotalProcessorTime } catch {}
      Start-Sleep -Milliseconds 20
    }
    try { $cpu = $p.TotalProcessorTime } catch {}
    "  [cpu {0:N2} s, peak {1:N0} MB]" -f $cpu.TotalSeconds, ($peak / 1MB)
  }

  foreach ($s in $Scenarios) {
    foreach ($r in 1..$Runs) {
      foreach ($c in $Clients) {
        "== $s run ${r}: $c"
        switch ($c) {
          "velt" { Measure-Client (Join-Path $work "client-velt.exe") @($base, $s) }
          "node" { Measure-Client "node" @((Join-Path $here "client.mjs"), $base, $s) }
          "reqwest" { Measure-Client (Join-Path $target "release/client.exe") @($base, $s) }
        }
      }
    }
  }
} finally {
  if ($server) { Stop-Process -Id $server.Id -ErrorAction SilentlyContinue }
  Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
