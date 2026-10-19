# Install a Velt dist directory (from scripts/package.ps1, or an unpacked release .zip) beside the
# other installed versions (#948, docs/tooling/platforms.md): as <root>\toolchains\<version>, with
# its launcher as <root>\bin\velt.exe. Replaces an installed copy of the same version (a rebuild).
# Does not modify PATH; prints the command to do so.
#
# Usage: pwsh scripts/install.ps1 -Dist <dist dir> [-Prefix <root>]
#   -Prefix  default: $env:LOCALAPPDATA\velt
#
# To use a build without installing it, link it instead: velt toolchain link dev <dist dir>.
param(
    [Parameter(Mandatory = $true)][string]$Dist,
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA "velt")
)
$ErrorActionPreference = "Stop"

$Dist = (Resolve-Path $Dist).Path
foreach ($f in @("bin/velt.exe", "bin/velt-launcher.exe", "lib/velt_rt.lib")) {
    if (-not (Test-Path (Join-Path $Dist $f) -PathType Leaf)) {
        throw "$Dist is not a Velt dist directory (missing $f)"
    }
}
# `velt <version> (<commit> <triple>)`
$Version = ((& (Join-Path $Dist "bin/velt.exe") --version) -split "\s+")[1]
if (-not $Version) { throw "cannot read the version of $Dist\bin\velt.exe" }

New-Item -ItemType Directory -Force (Join-Path $Prefix "toolchains"), (Join-Path $Prefix "bin") | Out-Null
$Root = (Resolve-Path $Prefix).Path
$Dest = Join-Path $Root "toolchains\$Version"
$Staging = Join-Path $Root "toolchains\.$Version.$PID"
if (Test-Path $Staging) { Remove-Item -Recurse -Force $Staging }
try {
    Copy-Item -Recurse $Dist $Staging
    if (Test-Path $Dest) {
        $Old = Join-Path $Root "toolchains\.$Version.old.$PID"
        Rename-Item $Dest $Old
        Remove-Item -Recurse -Force $Old -ErrorAction SilentlyContinue
    }
    Rename-Item $Staging $Dest
} finally {
    # A failed install leaves no partial toolchain behind.
    if (Test-Path $Staging) { Remove-Item -Recurse -Force $Staging -ErrorAction SilentlyContinue }
}
# A running velt.exe can be renamed but not overwritten.
$Launcher = Join-Path $Root "bin\velt.exe"
if (Test-Path $Launcher) {
    $Aside = Join-Path $Root ("bin\.velt.exe.old-" + [guid]::NewGuid().ToString("N"))
    Rename-Item $Launcher $Aside
    Remove-Item -Force $Aside -ErrorAction SilentlyContinue
}
Copy-Item (Join-Path $Dist "bin\velt-launcher.exe") $Launcher
$DefaultFile = Join-Path $Root "default"
if (-not ((Test-Path $DefaultFile) -and (Get-Content -Raw $DefaultFile).Trim())) {
    [IO.File]::WriteAllText($DefaultFile, "$Version`n")
}

$Bin = Join-Path $Root "bin"
Write-Host "installed velt $Version into $Dest (default: $((Get-Content -Raw $DefaultFile).Trim()))"
Write-Host ""
Write-Host "Add the launcher to your PATH (current user, persistent; open a new terminal afterwards):"
Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';$Bin', 'User')"
Write-Host "or for this session only:"
Write-Host "  `$env:Path += ';$Bin'"
Write-Host ""
Write-Host "Then check the installation with:  velt doctor"
