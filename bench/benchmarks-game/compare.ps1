# A/B gate over the Benchmarks Game programs (docs/design/semantics.md "Gates"), Windows: builds
# every <program>/main.vlt with two Velt compilers (LLVM release), runs them interleaved
# (A B A B ...) with the official N from bench.conf (stdin = the fasta output of that N when
# STDIN=fasta), checks that both print the same bytes, and prints the median wall-clock time.
#
#   pwsh bench/benchmarks-game/compare.ps1 -Baseline D:/base/release/velt.exe
#        [-Candidate <velt.exe>] [-Runs 5] [-Only n-body] [-Gate 3]
#
# Exits 1 if a program's median is more than -Gate percent slower than the baseline's.
param(
    [Parameter(Mandatory = $true)][string]$Baseline,
    [string]$Candidate = "",
    [int]$Runs = 5,
    [string]$Only = "",
    [double]$Gate = 3.0
)
$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
$root = Split-Path -Parent (Split-Path -Parent $here)
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
$out = Join-Path $targetDir "bench-game-compare"
New-Item -ItemType Directory -Force $out | Out-Null
if (-not $Candidate) { $Candidate = Join-Path $targetDir "release/velt.exe" }

function Conf([string]$dir, [string]$key) {
    $line = Get-Content (Join-Path $dir "bench.conf") | Where-Object { $_ -match "^$key=" } | Select-Object -First 1
    if (-not $line) { return "" }
    return ($line -replace "^$key=", "" -replace "\s*#.*$", "").Trim()
}

function Build([string]$velt, [string]$src, [string]$exe) {
    & $velt build --release --backend llvm $src -o $exe
    if ($LASTEXITCODE -ne 0) { throw "build failed: $src with $velt" }
}

# One timed run: (ms, output file).
function Time-Once([string]$exe, [string]$arg, [string]$stdin, [string]$outFile) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = if ($stdin) {
        Start-Process -FilePath $exe -ArgumentList $arg -RedirectStandardInput $stdin -RedirectStandardOutput $outFile -NoNewWindow -Wait -PassThru
    } else {
        Start-Process -FilePath $exe -ArgumentList $arg -RedirectStandardOutput $outFile -NoNewWindow -Wait -PassThru
    }
    $sw.Stop()
    if ($p.ExitCode -ne 0) { throw "$exe exited with $($p.ExitCode)" }
    return $sw.Elapsed.TotalMilliseconds
}

function Median([double[]]$xs) {
    $s = $xs | Sort-Object
    $n = $s.Count
    if ($n % 2 -eq 1) { return $s[($n - 1) / 2] }
    return ($s[$n / 2 - 1] + $s[$n / 2]) / 2
}

$fasta = Join-Path $out "fasta-base.exe"
Build $Baseline (Join-Path $here "fasta/main.vlt") $fasta
$failed = @()
Write-Output "| program | N | baseline median ms | candidate median ms | change |"
Write-Output "|---|---|---|---|---|"
foreach ($conf in Get-ChildItem $here -Recurse -Filter bench.conf | Sort-Object FullName) {
    $dir = $conf.DirectoryName
    $name = Split-Path -Leaf $dir
    if ($Only -and $name -ne $Only) { continue }
    $n = Conf $dir "N"
    $stdin = ""
    if ((Conf $dir "STDIN") -eq "fasta") {
        $stdin = Join-Path $out "fasta-$n.txt"
        if (-not (Test-Path $stdin)) { Time-Once $fasta $n "" $stdin | Out-Null }
    }
    $src = Join-Path $dir "main.vlt"
    $exeA = Join-Path $out "$name-base.exe"
    $exeB = Join-Path $out "$name-cand.exe"
    Build $Baseline $src $exeA
    Build $Candidate $src $exeB
    $outA = Join-Path $out "$name-a.out"
    $outB = Join-Path $out "$name-b.out"
    $a = @(); $b = @()
    for ($i = 0; $i -lt $Runs; $i++) {
        $a += Time-Once $exeA $n $stdin $outA
        $b += Time-Once $exeB $n $stdin $outB
        if ((Get-FileHash $outA).Hash -ne (Get-FileHash $outB).Hash) { throw "$name`: outputs differ" }
    }
    $ma = Median $a; $mb = Median $b
    $pct = ($mb - $ma) / $ma * 100
    if ($pct -gt $Gate) { $failed += $name }
    Write-Output ("| {0} | {1} | {2:N1} | {3:N1} | {4:+0.0;-0.0}% |" -f $name, $n, $ma, $mb, $pct)
}
Write-Output ""
Write-Output "Median of $Runs interleaved runs each, wall-clock ms including process start (LLVM release)."
if ($failed.Count -gt 0) {
    Write-Output "SLOWER than the +$Gate% gate: $($failed -join ', ')"
    exit 1
}
