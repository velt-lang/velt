# CPU time of a program's own process (user + kernel, from GetProcessTimes through .NET), not of
# the children it starts: for A/Bs of programs that start children on Windows, where cachegrind
# does not run. Runs the executables interleaved (A B A B ...) and prints the best and the median
# of each, and the change of the best.
#
#   pwsh bench/exec_sync/cpu.ps1 -A base.exe -B head.exe [-Runs 11]
param(
    [Parameter(Mandatory = $true)][string]$A,
    [Parameter(Mandatory = $true)][string]$B,
    [int]$Runs = 11
)
$ErrorActionPreference = "Stop"

function Cpu-Once([string]$exe) {
    $info = New-Object Diagnostics.ProcessStartInfo $exe
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $p = [Diagnostics.Process]::Start($info)
    $text = $p.StandardOutput.ReadToEnd()
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) { throw "$exe exited with $($p.ExitCode)" }
    $ms = $p.TotalProcessorTime.TotalMilliseconds
    $p.Dispose()
    return @{ ms = $ms; out = $text }
}

function Median([double[]]$xs) {
    $s = $xs | Sort-Object
    return $s[[int][Math]::Floor($s.Count / 2)]
}

$ta = @(); $tb = @()
for ($i = 0; $i -lt $Runs; $i++) {
    $ra = Cpu-Once $A
    $rb = Cpu-Once $B
    if ($ra.out -ne $rb.out) { throw "outputs differ: [$($ra.out)] vs [$($rb.out)]" }
    $ta += $ra.ms; $tb += $rb.ms
}
$ba = ($ta | Measure-Object -Minimum).Minimum
$bb = ($tb | Measure-Object -Minimum).Minimum
"A: best {0:N1} ms, median {1:N1} ms" -f $ba, (Median $ta)
"B: best {0:N1} ms, median {1:N1} ms" -f $bb, (Median $tb)
"change of the best: {0:+0.0;-0.0}%" -f (100.0 * ($bb - $ba) / $ba)
