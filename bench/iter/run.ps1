# Iteration benchmark harness (Windows / PowerShell 7): builds every bench/iter/*.vlt (LLVM
# release), checks that each prints what its Node version (node/<name>.js) prints, and prints a
# Markdown table of the best wall-clock time over -Runs interleaved rounds, with each program's
# time relative to hand_loop. See run.sh for what the programs compare.
#
#   pwsh bench/iter/run.ps1 [-Runs 5]
param([int]$Runs = 5)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$metadata = cargo metadata --format-version 1 --no-deps --manifest-path (Join-Path $root "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed" }
$target = ($metadata | ConvertFrom-Json).target_directory
$out = Join-Path $target "bench-iter"
New-Item -ItemType Directory -Force $out | Out-Null

Write-Host "building velt (release) and the runtime..."
cargo build --release -q -p veltc -p velt_rt --manifest-path (Join-Path $root "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$velt = Join-Path $target "release/velt.exe"

$names = @()
foreach ($src in Get-ChildItem (Join-Path $PSScriptRoot "*.vlt")) {
    $name = $src.BaseName
    $names += $name
    & $velt build --release --backend llvm $src.FullName -o (Join-Path $out "$name.exe")
    if ($LASTEXITCODE -ne 0) { throw "build of $name failed" }
}

$best = @{}
$outputs = @{}
foreach ($n in $names) { $best[$n] = [double]::MaxValue }
for ($r = 0; $r -lt $Runs; $r++) {
    foreach ($n in $names) {
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $outputs[$n] = (& (Join-Path $out "$n.exe")) -join "`n"
        $sw.Stop()
        $best[$n] = [Math]::Min($best[$n], $sw.Elapsed.TotalMilliseconds)
    }
}
"| benchmark | Velt LLVM release (ms) | vs hand_loop | Node (ms) |"
"|---|---|---|---|"
foreach ($n in $names) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $nodeOut = (& node (Join-Path $PSScriptRoot "node/$n.js")) -join "`n"
    $sw.Stop()
    if ($nodeOut -ne $outputs[$n]) { throw "${n}: Velt and Node print different results" }
    $rel = "{0:N2}x" -f ($best[$n] / $best["hand_loop"])
    "| $n | $([Math]::Round($best[$n])) | $rel | $([Math]::Round($sw.Elapsed.TotalMilliseconds)) |"
}
