# cpu.ps1 <exe> <args...>: runs the command to completion and prints its CPU milliseconds
# (user + system, all threads). Used by cpu.sh on Windows.
param([string]$exe, [Parameter(ValueFromRemainingArguments = $true)][string[]]$rest)
$psi = New-Object System.Diagnostics.ProcessStartInfo $exe
foreach ($a in $rest) { $psi.ArgumentList.Add($a) }
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false
$p = [Diagnostics.Process]::Start($psi)
$null = $p.StandardOutput.ReadToEnd()
$p.WaitForExit()
[int]$p.TotalProcessorTime.TotalMilliseconds
