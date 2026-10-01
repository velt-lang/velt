# Async benchmark harness (Windows / PowerShell 7): builds every bench/async/*.vlt (LLVM release)
# plus the Rust/tokio (bench/async/rust) and Node (bench/async/node) versions, checks that all of
# them print the same output, and prints two Markdown tables: best wall-clock time over -Runs runs,
# and peak working set (max over the runs).
#
#   pwsh bench/async/run.ps1 [-Runs 5] [-Only spawn_many]
#
# Columns: Velt on all cores (default runtime) and with VELT_THREADS=1, Rust tokio multi-thread
# and current-thread runtimes, Node. Honors CARGO_TARGET_DIR. A configuration whose first run
# takes over 10 s is not repeated.
param(
    [int]$Runs = 5,
    [string]$Only = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
$out = Join-Path $target "bench-async"
New-Item -ItemType Directory -Force $out | Out-Null

Write-Host "building velt (release), the runtime and the tokio benchmarks..."
cargo build --release -q -p veltc -p velt_rt --manifest-path (Join-Path $root "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
cargo build --release -q --manifest-path (Join-Path $PSScriptRoot "rust/Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo build of bench/async/rust failed" }
$velt = Join-Path $target "release/velt.exe"

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class BenchPeakMemory {
    [StructLayout(LayoutKind.Sequential)]
    struct Counters {
        public uint cb; public uint PageFaultCount;
        public UIntPtr PeakWorkingSetSize; public UIntPtr WorkingSetSize;
        public UIntPtr QuotaPeakPagedPoolUsage; public UIntPtr QuotaPagedPoolUsage;
        public UIntPtr QuotaPeakNonPagedPoolUsage; public UIntPtr QuotaNonPagedPoolUsage;
        public UIntPtr PagefileUsage; public UIntPtr PeakPagefileUsage;
    }
    [DllImport("psapi.dll", SetLastError = true)]
    static extern bool GetProcessMemoryInfo(IntPtr process, out Counters counters, uint cb);
    // Peak working set in bytes of a (possibly exited) process whose handle is still open.
    public static long Peak(IntPtr process) {
        Counters c;
        if (!GetProcessMemoryInfo(process, out c, (uint)Marshal.SizeOf(typeof(Counters)))) return -1;
        return (long)c.PeakWorkingSetSize.ToUInt64();
    }
}
'@

# One run: @{ ms; out; peak } (peak working set in bytes).
function Invoke-Once([string]$exe, [string[]]$arguments, [hashtable]$envVars) {
    $psi = [Diagnostics.ProcessStartInfo]::new($exe)
    foreach ($a in $arguments) { $psi.ArgumentList.Add($a) }
    foreach ($k in $envVars.Keys) { $psi.Environment[$k] = $envVars[$k] }
    $psi.RedirectStandardOutput = $true
    $psi.UseShellExecute = $false
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = [Diagnostics.Process]::Start($psi)
    $text = $p.StandardOutput.ReadToEnd()
    $p.WaitForExit()
    $sw.Stop()
    if ($p.ExitCode -ne 0) { throw "$exe $arguments exited with $($p.ExitCode)" }
    return @{ ms = $sw.Elapsed.TotalMilliseconds; out = $text.Trim(); peak = [BenchPeakMemory]::Peak($p.Handle) }
}

function Measure-Best([string]$exe, [string[]]$arguments = @(), [hashtable]$envVars = @{}) {
    $best = @{ ms = [double]::MaxValue; peak = 0; out = "" }
    for ($i = 0; $i -lt $Runs; $i++) {
        $r = Invoke-Once $exe $arguments $envVars
        $best.ms = [Math]::Min($best.ms, $r.ms)
        $best.peak = [Math]::Max($best.peak, $r.peak)
        $best.out = $r.out
        if ($r.ms -gt 10000) { break }
    }
    return $best
}

$rustDir = Join-Path $target "release"
$columns = @("Velt (all cores)", "Velt (1 thread)", "Rust tokio multi-thread", "Rust tokio current-thread", "Node")
$rows = @()
foreach ($src in Get-ChildItem $PSScriptRoot -Filter *.vlt | Sort-Object Name) {
    $name = $src.BaseName
    if ($Only -and $name -ne $Only) { continue }
    Write-Host "$name..."
    $exe = Join-Path $out "$name.exe"
    & $velt build --release $src.FullName -o $exe
    if ($LASTEXITCODE -ne 0) { throw "velt build failed on $name" }
    $rustExe = Join-Path $rustDir "$name.exe"
    $results = [ordered]@{
        "Velt (all cores)"          = Measure-Best $exe
        "Velt (1 thread)"           = Measure-Best $exe @() @{ VELT_THREADS = "1" }
        "Rust tokio multi-thread"   = Measure-Best $rustExe
        "Rust tokio current-thread" = Measure-Best $rustExe @("current")
        "Node"                      = Measure-Best "node" @((Join-Path $PSScriptRoot "node/$name.js"))
    }
    $reference = $results["Rust tokio multi-thread"].out
    foreach ($k in $results.Keys) {
        if ($results[$k].out -ne $reference) { throw "$name / ${k}: output differs from Rust:`n$($results[$k].out)" }
    }
    $rows += , @($name, $results)
}

function Write-Table([string]$title, [scriptblock]$cell) {
    Write-Output $title
    Write-Output ""
    Write-Output ("| benchmark | " + ($columns -join " | ") + " |")
    Write-Output ("|---|" + (($columns | ForEach-Object { "---" }) -join "|") + "|")
    foreach ($row in $rows) {
        $values = @($row[0]) + @($columns | ForEach-Object { & $cell $row[1][$_] })
        Write-Output ("| " + ($values -join " | ") + " |")
    }
    Write-Output ""
}
Write-Table "Best of $Runs runs, wall-clock ms including process start:" { param($r) [int][Math]::Round($r.ms) }
Write-Table "Peak working set, MB:" { param($r) ([Math]::Round($r.peak / 1MB, 1)).ToString([Globalization.CultureInfo]::InvariantCulture) }
