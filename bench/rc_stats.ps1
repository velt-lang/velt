# Refcount counters per benchmark (docs/internals/design/semantics.md "Gates"): builds every bench/*.vlt
# (and bench/async/*.vlt) with the release compiler in LLVM release mode, links the *debug*
# runtime (the only one with counters) and runs it with VELT_RC_STATS=1.
#
#   pwsh bench/rc_stats.ps1        # needs `cargo build -p velt_rt` and `cargo build --release -p veltc`
param([string]$Only = "")
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
$velt = Join-Path $target "release/velt.exe"
$rtName = if ($IsWindows) { "velt_rt.lib" } else { "libvelt_rt.a" }
$env:VELT_RT_LIB = Join-Path $target "debug/$rtName"
$out = Join-Path $target "bench-rc"
New-Item -ItemType Directory -Force $out | Out-Null
Write-Output "| benchmark | retain | release | heap alloc | heap free |"
Write-Output "|---|---|---|---|---|"
$srcs = @(Get-ChildItem (Join-Path $root "bench") -Filter *.vlt) + @(Get-ChildItem (Join-Path $root "bench/async") -Filter *.vlt)
foreach ($src in $srcs | Sort-Object Name) {
    if ($Only -and $src.BaseName -ne $Only) { continue }
    $exe = Join-Path $out "$($src.BaseName).exe"
    & $velt build --release --backend llvm $src.FullName -o $exe
    if ($LASTEXITCODE -ne 0) { throw "build failed: $($src.Name)" }
    $env:VELT_RC_STATS = "1"
    $line = (& $exe 2>&1 | Select-String "rc stats:" | Select-Object -Last 1).ToString()
    Remove-Item Env:VELT_RC_STATS
    $n = [regex]::Matches($line, "=(\d+)") | ForEach-Object { $_.Groups[1].Value }
    Write-Output ("| {0} | {1} | {2} | {3} | {4} |" -f $src.BaseName, $n[0], $n[1], $n[2], $n[3])
}
Remove-Item Env:VELT_RT_LIB
