# cpu.ps1 <exe> <args...>: runs the command to completion and prints "<cpu ms> <user ms> <wall ms>"
# (CPU = user + system, all threads). Used by cpu.sh on Windows.
param([string]$exe, [Parameter(ValueFromRemainingArguments = $true)][string[]]$rest)
$psi = New-Object System.Diagnostics.ProcessStartInfo $exe
foreach ($a in $rest) { $psi.ArgumentList.Add($a) }
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = [Diagnostics.Process]::Start($psi)
$null = $p.StandardOutput.ReadToEnd()
$p.WaitForExit()
"{0:F0} {1:F0} {2:F0}" -f $p.TotalProcessorTime.TotalMilliseconds, $p.UserProcessorTime.TotalMilliseconds, $sw.Elapsed.TotalMilliseconds
