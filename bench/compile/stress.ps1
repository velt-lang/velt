# Compile-time memory stress run (Windows / PowerShell 7; stress.sh is the Linux / macOS
# counterpart): a debug build (Cranelift) of long_main_<Classes> (-Classes classes with an override
# and a generic instance each, all used from one `main` of 6 × Classes blocks; the same program as
# run.ps1's long_main) must stay under -LimitMB of peak memory (peak working set of `velt build`).
# Prints the stage times, the peak and the verdict; exits 1 over the limit.
#
#   pwsh bench/compile/stress.ps1 [-Classes 16000] [-LimitMB 2048] [-Velt <path to velt>]
#
# Without -Velt it builds velt (release) first.
param(
    [int]$Classes = 16000,
    [int]$LimitMB = 2048,
    [string]$Velt = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
$out = Join-Path $target "bench/compile"
New-Item -ItemType Directory -Force $out | Out-Null

if (-not $Velt) {
    Write-Host "building velt and the runtimes (release)..."
    cargo build --release -q -p veltc -p velt_rt -p velt_rt_shared --manifest-path (Join-Path $root "Cargo.toml")
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    $Velt = Join-Path $target "release/velt.exe"
}

$sb = [Text.StringBuilder]::new("function count<T>(xs: T[]): i64 {`n  return xs.length as i64;`n}`n`n")
for ($i = 0; $i -lt $Classes; $i++) {
    [void]$sb.Append("class C$i {`n  id: i64;`n  constructor(id: i64) {`n    this.id = id;`n  }`n  area(): i64 {`n    return this.id;`n  }`n}`n`n")
    [void]$sb.Append("class S$i extends C$i {`n  constructor(id: i64) {`n    super(id);`n  }`n  override area(): i64 {`n    return this.id + 1;`n  }`n}`n`n")
}
[void]$sb.Append("function main() {`n  let t = 0;`n")
for ($i = 0; $i -lt $Classes; $i++) {
    [void]$sb.Append("  const b${i}: C$i = new S$i($i);`n  t += b$i.area() + count([b$i]);`n")
}
[void]$sb.Append("  console.log(t);`n}`n")
$file = Join-Path $out "long_main_$Classes.vlt"
[IO.File]::WriteAllText($file, $sb.ToString())
$exe = Join-Path $out "long_main_${Classes}_stress"
Remove-Item -ErrorAction SilentlyContinue "$exe.exe", "$exe.exe.link-stamp"

# Poll the peak working set while the build runs (it is not readable after the process exits).
$log = Join-Path $out "stress.log"
$proc = Start-Process -FilePath $Velt -ArgumentList @("build", "-v", "`"$file`"", "-o", "`"$exe`"") `
    -NoNewWindow -PassThru -RedirectStandardError $log
$peak = 0
while (-not $proc.HasExited) {
    $proc.Refresh()
    try { $peak = [Math]::Max($peak, $proc.PeakWorkingSet64) } catch { }
    Start-Sleep -Milliseconds 50
}
$proc.WaitForExit()
Get-Content $log | Write-Host
if ($proc.ExitCode -ne 0) { throw "velt build $file failed" }

$expected = [long]0
for ($i = 0; $i -lt $Classes; $i++) { $expected += $i + 2 }
$got = (& "$exe.exe" | Out-String).Trim()
if ($got -ne "$expected") { throw "long_main_$Classes printed '$got', expected '$expected'" }
$peakMB = [Math]::Round($peak / 1MB)
$verdict = if ($peakMB -le $LimitMB) { "ok" } else { "OVER THE LIMIT" }
Write-Output "long_main_$Classes, debug build: peak memory $peakMB MB (limit $LimitMB MB): $verdict"
if ($peakMB -gt $LimitMB) { exit 1 }
