# A/B gate for language changes (docs/internals/design/semantics.md "Gates"): builds every bench/*.vlt
# with two Velt compilers (LLVM release), runs the executables interleaved (A B A B ...) so both
# see the same machine state, and prints the median wall-clock time of each and the change.
#
#   pwsh bench/compare.ps1 -Baseline D:/base/target/release/velt.exe [-Candidate <velt.exe>]
#                          [-Runs 11] [-Only strings] [-Gate 3] [-Dir bench/async]
#
# -Candidate defaults to this checkout's release compiler ($env:CARGO_TARGET_DIR respected).
# Exits 1 if any benchmark's median is more than -Gate percent slower than the baseline's.
# Outputs must match between A and B. The baseline compiler finds its own std/ and runtime.
param(
    [Parameter(Mandatory = $true)][string]$Baseline,
    [string]$Candidate = "",
    [int]$Runs = 11,
    [string]$Only = "",
    [double]$Gate = 3.0,
    [string]$Dir = "bench"
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
$out = Join-Path $targetDir "bench-compare"
New-Item -ItemType Directory -Force $out | Out-Null
if (-not $Candidate) {
    Write-Host "building velt (release) and the runtime..."
    cargo build --release -q -p veltc -p velt_rt --manifest-path (Join-Path $root "Cargo.toml")
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    $Candidate = Join-Path $targetDir "release/velt.exe"
}

function Time-Once([string]$exe) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $text = (& $exe) -join "`n"
    $sw.Stop()
    if ($LASTEXITCODE -ne 0) { throw "$exe exited with $LASTEXITCODE" }
    return @{ ms = $sw.Elapsed.TotalMilliseconds; out = $text }
}

function Median([double[]]$xs) {
    $s = $xs | Sort-Object
    $n = $s.Count
    if ($n % 2 -eq 1) { return $s[($n - 1) / 2] }
    return ($s[$n / 2 - 1] + $s[$n / 2]) / 2
}

$failed = @()
Write-Output "| benchmark | baseline median ms | candidate median ms | change |"
Write-Output "|---|---|---|---|"
foreach ($src in Get-ChildItem (Join-Path $root $Dir) -Filter *.vlt | Sort-Object Name) {
    $name = $src.BaseName
    if ($Only -and $name -ne $Only) { continue }
    $exeA = Join-Path $out "$name-base.exe"
    $exeB = Join-Path $out "$name-cand.exe"
    & $Baseline build --release --backend llvm $src.FullName -o $exeA
    if ($LASTEXITCODE -ne 0) { throw "baseline build failed: $name" }
    & $Candidate build --release --backend llvm $src.FullName -o $exeB
    if ($LASTEXITCODE -ne 0) { throw "candidate build failed: $name" }
    $a = @(); $b = @()
    $refOut = $null
    for ($i = 0; $i -lt $Runs; $i++) {
        $ra = Time-Once $exeA
        $rb = Time-Once $exeB
        if ($ra.out -ne $rb.out) { throw "$name`: outputs differ`n--- baseline`n$($ra.out)`n--- candidate`n$($rb.out)" }
        $a += $ra.ms; $b += $rb.ms
    }
    $ma = Median $a; $mb = Median $b
    $pct = ($mb - $ma) / $ma * 100
    if ($pct -gt $Gate) { $failed += $name }
    Write-Output ("| {0} | {1:N1} | {2:N1} | {3:+0.0;-0.0}% |" -f $name, $ma, $mb, $pct)
}
Write-Output ""
Write-Output "Median of $Runs interleaved runs each, wall-clock ms including process start (LLVM release)."
if ($failed.Count -gt 0) {
    Write-Output "SLOWER than the +$Gate% gate: $($failed -join ', ')"
    exit 1
}
