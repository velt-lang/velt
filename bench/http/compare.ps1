# HTTP A/B gate (docs/internals/design/semantics.md "Gates"): builds examples/http_hello.vlt with two Velt
# compilers (LLVM release), serves it on 127.0.0.1:8080 and loads it with oha, alternating A and B
# for -Rounds rounds. Prints req/s and peak working set per round and the medians. With -SoakSeconds
# it then runs the candidate alone under load for that long, sampling its working set every
# -SampleSeconds (leak check: the working set must not grow).
#
#   pwsh bench/http/compare.ps1 -Baseline D:/base/target/release/velt.exe [-Candidate <velt.exe>]
#        [-Rounds 3] [-Seconds 10] [-Conns 256] [-SoakSeconds 600] [-SampleSeconds 30]
#
# Needs oha on PATH. The client runs on the same machine (it competes for CPU).
param(
    [Parameter(Mandatory = $true)][string]$Baseline,
    [string]$Candidate = "",
    [int]$Rounds = 3,
    [int]$Seconds = 10,
    [int]$Conns = 256,
    [int]$SoakSeconds = 0,
    [int]$SampleSeconds = 30
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
if (-not $Candidate) { $Candidate = Join-Path $target "release/velt.exe" }
$out = Join-Path $target "bench-http"
New-Item -ItemType Directory -Force $out | Out-Null
$src = Join-Path $root "examples/http_hello.vlt"
$exes = @{ baseline = (Join-Path $out "hello-base.exe"); candidate = (Join-Path $out "hello-cand.exe") }
& $Baseline build --release --backend llvm $src -o $exes.baseline
if ($LASTEXITCODE -ne 0) { throw "baseline build failed" }
& $Candidate build --release --backend llvm $src -o $exes.candidate
if ($LASTEXITCODE -ne 0) { throw "candidate build failed" }

function Start-Server([string]$exe) {
    $p = Start-Process -FilePath $exe -PassThru -WindowStyle Hidden -RedirectStandardOutput (Join-Path $out "server.log")
    for ($i = 0; $i -lt 100; $i++) {
        try { Invoke-WebRequest -UseBasicParsing http://127.0.0.1:8080/ -TimeoutSec 1 | Out-Null; return $p } catch { Start-Sleep -Milliseconds 100 }
    }
    throw "server $exe did not start"
}

function Load([int]$secs) {
    $json = oha -z "${secs}s" -c $Conns --no-tui --output-format json http://127.0.0.1:8080/ | ConvertFrom-Json
    return $json.summary.requestsPerSec
}

function Median([double[]]$xs) { $s = $xs | Sort-Object; return $s[[int][Math]::Floor(($s.Count - 1) / 2)] }

$rps = @{ baseline = @(); candidate = @() }
$rss = @{ baseline = @(); candidate = @() }
Write-Output "| round | server | req/s | peak working set MB |"
Write-Output "|---|---|---|---|"
for ($r = 1; $r -le $Rounds; $r++) {
    foreach ($k in "baseline", "candidate") {
        $p = Start-Server $exes[$k]
        $q = Load $Seconds
        $p.Refresh()
        $peak = $p.PeakWorkingSet64 / 1MB
        Stop-Process -Id $p.Id -Force
        $p.WaitForExit()
        $rps[$k] += $q; $rss[$k] += $peak
        Write-Output ("| {0} | {1} | {2:N0} | {3:N1} |" -f $r, $k, $q, $peak)
        Start-Sleep -Seconds 2
    }
}
Write-Output ""
foreach ($k in "baseline", "candidate") {
    Write-Output ("median {0}: {1:N0} req/s, {2:N1} MB peak" -f $k, (Median $rps[$k]), (Median $rss[$k]))
}

if ($SoakSeconds -gt 0) {
    Write-Output ""
    Write-Output "soak: candidate, $SoakSeconds s, $Conns connections, working set every $SampleSeconds s"
    $p = Start-Server $exes.candidate
    $job = Start-Job -ScriptBlock { param($s, $c) oha -z "${s}s" -c $c --no-tui --output-format json http://127.0.0.1:8080/ } -ArgumentList $SoakSeconds, $Conns
    $t0 = Get-Date
    while ($job.State -eq "Running") {
        Start-Sleep -Seconds $SampleSeconds
        $p.Refresh()
        $el = [int]((Get-Date) - $t0).TotalSeconds
        Write-Output ("t={0,5}s working set {1:N1} MB, private {2:N1} MB" -f $el, ($p.WorkingSet64 / 1MB), ($p.PrivateMemorySize64 / 1MB))
    }
    $res = Receive-Job $job | ConvertFrom-Json
    $p.Refresh()
    Write-Output ("soak done: {0:N0} req/s, {1:N0} requests, final working set {2:N1} MB, peak {3:N1} MB" -f $res.summary.requestsPerSec, ($res.summary.requestsPerSec * $SoakSeconds), ($p.WorkingSet64 / 1MB), ($p.PeakWorkingSet64 / 1MB))
    Stop-Process -Id $p.Id -Force
}
