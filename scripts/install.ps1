# Install a Velt dist directory (from scripts/package.ps1, or an unpacked release .zip) into a prefix.
# Does not modify PATH; prints the command to do so.
#
# Usage: pwsh scripts/install.ps1 -Dist <dist dir> [-Prefix <dir>]
#   -Prefix  default: $env:LOCALAPPDATA\velt
param(
    [Parameter(Mandatory = $true)][string]$Dist,
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA "velt")
)
$ErrorActionPreference = "Stop"

$Dist = (Resolve-Path $Dist).Path
foreach ($f in @("bin/velt.exe", "lib/velt_rt.lib")) {
    if (-not (Test-Path (Join-Path $Dist $f) -PathType Leaf)) {
        throw "$Dist is not a Velt dist directory (missing $f)"
    }
}

New-Item -ItemType Directory -Force $Prefix | Out-Null
$Prefix = (Resolve-Path $Prefix).Path
# Replace the toolchain parts wholesale so files removed upstream (e.g. std modules) disappear.
foreach ($d in @("bin", "lib", "std", "share\velt")) {
    $dest = Join-Path $Prefix $d
    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
    if (Test-Path (Join-Path $Dist $d)) {
        New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
        Copy-Item -Recurse (Join-Path $Dist $d) $dest
    }
}
foreach ($f in @("README.md", "LICENSE-MIT", "LICENSE-APACHE", "NOTICE")) {
    if (Test-Path (Join-Path $Dist $f)) { Copy-Item -Force (Join-Path $Dist $f) $Prefix }
}

$Bin = Join-Path $Prefix "bin"
Write-Host "installed Velt into $Prefix"
Write-Host ""
Write-Host "Add it to your PATH (current user, persistent; open a new terminal afterwards):"
Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';$Bin', 'User')"
Write-Host "or for this session only:"
Write-Host "  `$env:Path += ';$Bin'"
Write-Host ""
Write-Host "Then check the installation with:  velt doctor"
