# Benchmark harness (Windows / PowerShell 7): builds every bench/*.vlt with Velt' three
# configurations plus the Rust (rustc -O) and Node equivalents, checks that all of them print
# the same output, and prints a Markdown table of the best wall-clock time over -Runs runs.
#
#   pwsh bench/run.ps1 [-Runs 5] [-Only fib]
#
# Needs: cargo, rustc, node on PATH; clang for the LLVM column (else it shows "n/a").
# Async benchmarks (tokio baselines, peak memory) have their own harness: bench/async/run.ps1.
param(
    [int]$Runs = 5,
    [string]$Only = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
$out = Join-Path $target "bench"
New-Item -ItemType Directory -Force $out | Out-Null

Write-Host "building velt (release) and the runtime..."
cargo build --release -q -p veltc -p velt_rt --manifest-path (Join-Path $root "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$velt = Join-Path $target "release/velt.exe"

function Measure-Best([string[]]$cmd) {
    $best = [double]::MaxValue
    $text = ""
    for ($i = 0; $i -lt $Runs; $i++) {
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $text = (& $cmd[0] @($cmd | Select-Object -Skip 1)) -join "`n"
        $sw.Stop()
        if ($LASTEXITCODE -ne 0) { throw "$($cmd -join ' ') exited with $LASTEXITCODE" }
        $best = [Math]::Min($best, $sw.Elapsed.TotalMilliseconds)
    }
    return @{ ms = $best; out = $text }
}

$configs = @(
    @{ name = "Velt cranelift debug"; args = @() },
    @{ name = "Velt cranelift release (+velt_opt)"; args = @("--release", "--backend", "cranelift") },
    @{ name = "Velt LLVM release"; args = @("--release", "--backend", "llvm") }
)
$rows = @()
foreach ($src in Get-ChildItem (Join-Path $root "bench") -Filter *.vlt | Sort-Object Name) {
    $name = $src.BaseName
    if ($Only -and $name -ne $Only) { continue }
    $cells = [ordered]@{}
    $rustExe = Join-Path $out "$name-rust.exe"
    rustc -O --edition 2021 -o $rustExe (Join-Path $root "bench/rust/$name.rs")
    if ($LASTEXITCODE -ne 0) { throw "rustc failed on $name" }
    $reference = Measure-Best @($rustExe)
    foreach ($c in $configs) {
        $exe = Join-Path $out "$name-$($configs.IndexOf($c)).exe"
        & $velt build @($c.args) $src.FullName -o $exe 2>$null
        if ($LASTEXITCODE -ne 0) { $cells[$c.name] = "n/a"; continue }
        $r = Measure-Best @($exe)
        if ($r.out -ne $reference.out) { throw "$name / $($c.name): output differs from Rust:`n$($r.out)" }
        $cells[$c.name] = [int][Math]::Round($r.ms)
    }
    $cells["Rust -O"] = [int][Math]::Round($reference.ms)
    $node = Measure-Best @("node", (Join-Path $root "bench/node/$name.js"))
    if ($node.out -ne $reference.out) { throw "$name / node: output differs from Rust:`n$($node.out)" }
    $cells["Node"] = [int][Math]::Round($node.ms)
    $rows += , @($name, $cells)
}

$headers = @("benchmark") + ($configs | ForEach-Object { $_.name }) + @("Rust -O", "Node")
Write-Output ("| " + ($headers -join " | ") + " |")
Write-Output ("|" + (($headers | ForEach-Object { "---" }) -join "|") + "|")
foreach ($row in $rows) {
    $values = @($row[0]) + @($headers | Select-Object -Skip 1 | ForEach-Object { $row[1][$_] })
    Write-Output ("| " + ($values -join " | ") + " |")
}
Write-Output ""
Write-Output "Best of $Runs runs, wall-clock milliseconds including process start."
