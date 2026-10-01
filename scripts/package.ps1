# Build a release Velt toolchain and assemble dist/velt-<version>-<host triple>/ (+ .zip).
#
# Layout (see docs/tooling/platforms.md):
#   bin/velt.exe  lib/velt_rt.lib  lib/velt_rt_shared.dll(.lib)  lib/NATIVE_LIBS.md  std/**  README.md  LICENSE-MIT  LICENSE-APACHE
#
# Usage: pwsh scripts/package.ps1 [-StdDir <dir>] [-SkipBuild] [-NoArchive]
#   -StdDir     std sources to ship (default: <repo>/std)
#   -SkipBuild  reuse the existing release build
#   -NoArchive  only assemble the directory
param(
    [string]$StdDir = "",
    [switch]$SkipBuild,
    [switch]$NoArchive
)
$ErrorActionPreference = "Stop"

$Repo = Split-Path -Parent $PSScriptRoot
if (-not $StdDir) { $StdDir = Join-Path $Repo "std" }
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Repo "target" }

$Version = (Select-String -Path (Join-Path $Repo "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$HostTriple = ((rustc -vV) | Where-Object { $_ -like "host:*" }) -replace "^host:\s*", ""
if (-not $Version -or -not $HostTriple) { throw "could not determine the version or host triple" }

if (-not $SkipBuild) {
    Write-Host "building release velt + velt_rt + velt_rt_shared..."
    cargo build --release -p veltc -p velt_rt -p velt_rt_shared --manifest-path (Join-Path $Repo "Cargo.toml")
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

$Release = Join-Path $TargetDir "release"
$Exe = Join-Path $Release "velt.exe"
$RtLib = Join-Path $Release "velt_rt.lib"
# The shared runtime debug builds link (crates/velt_rt_shared): the DLL and its import library.
$SharedRt = @((Join-Path $Release "velt_rt_shared.dll"), (Join-Path $Release "velt_rt_shared.dll.lib"))
foreach ($f in @($Exe, $RtLib) + $SharedRt) {
    if (-not (Test-Path $f -PathType Leaf)) { throw "missing build output: $f" }
}

$Name = "velt-$Version-$HostTriple"
$Dist = Join-Path $Repo "dist"
$Out = Join-Path $Dist $Name
if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
foreach ($d in @("bin", "lib", "std")) { New-Item -ItemType Directory -Force (Join-Path $Out $d) | Out-Null }

Copy-Item $Exe (Join-Path $Out "bin")
Copy-Item $RtLib (Join-Path $Out "lib")
foreach ($f in $SharedRt) { Copy-Item $f (Join-Path $Out "lib") }
Copy-Item (Join-Path $Repo "crates/velt_rt/NATIVE_LIBS.md") (Join-Path $Out "lib")
if (Test-Path $StdDir -PathType Container) {
    Copy-Item -Recurse (Join-Path $StdDir "*") (Join-Path $Out "std")
} else {
    Write-Warning "no std sources at $StdDir; shipping an empty std/ (pass -StdDir)"
}

@"
# Velt $Version ($HostTriple)

Install:  pwsh scripts/install.ps1 -Dist <this directory>   (or copy it anywhere)
Then add ``<prefix>\bin`` to PATH and run ``velt doctor``.

    velt run hello.vlt
    velt new app; cd app; velt run

Layout: bin/ (the velt CLI), lib/ (runtime library linked into every program),
std/ (standard library sources). Full guide: docs/tooling/platforms.md in the Velt repository.
"@ | Set-Content -Encoding utf8 (Join-Path $Out "README.md")

Copy-Item (Join-Path $Repo "LICENSE-MIT"), (Join-Path $Repo "LICENSE-APACHE") $Out

if (-not $NoArchive) {
    $Zip = Join-Path $Dist "$Name.zip"
    if (Test-Path $Zip) { Remove-Item -Force $Zip }
    Compress-Archive -Path $Out -DestinationPath $Zip
    Write-Host "archive: $Zip"
}
Write-Host "dist:    $Out"
