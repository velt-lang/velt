# Build a release Velt toolchain and assemble dist/velt-<version>-<host triple>/ (+ .zip).
#
# Layout (see docs/tooling/platforms.md):
#   bin/velt.exe  lib/velt_rt.lib  lib/velt_rt_shared.dll(.lib)  lib/NATIVE_LIBS.md
#   lib/velt/lld.exe  lib/targets/<triple>/ (link kit)
#   std/**  README.md  LICENSE-MIT  LICENSE-APACHE  NOTICE
#
# With the bundled linker, velt.exe and velt_rt_shared.dll are linked with it too (lld-link, the
# kit's startup object and the Universal CRT), so neither needs the Visual C++ runtime
# (vcruntime140.dll) on the machine they run on.
#
# Usage: pwsh scripts/package.ps1 [-StdDir <dir>] [-SkipBuild] [-NoArchive] [-Lld <path>] [-NoBundledLinker]
#   -StdDir           std sources to ship (default: <repo>/std)
#   -SkipBuild        reuse the existing release build
#   -NoArchive        only assemble the directory
#   -Lld              the lld.exe to bundle (scripts/build-lld.ps1 builds one; default: build it
#                     once into $env:VELT_LLD_CACHE, %LOCALAPPDATA%\velt\lld-<LLVM version>)
#   -NoBundledLinker  ship no lld and no link kit (programs link with link.exe)
param(
    [string]$StdDir = "",
    [switch]$SkipBuild,
    [switch]$NoArchive,
    [string]$Lld = $env:VELT_LLD,
    [switch]$NoBundledLinker
)
$ErrorActionPreference = "Stop"

$Repo = Split-Path -Parent $PSScriptRoot
if (-not $StdDir) { $StdDir = Join-Path $Repo "std" }
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Repo "target" }

$Version = (Select-String -Path (Join-Path $Repo "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$HostTriple = ((rustc -vV) | Where-Object { $_ -like "host:*" }) -replace "^host:\s*", ""
if (-not $Version -or -not $HostTriple) { throw "could not determine the version or host triple" }

$Manifest = Join-Path $Repo "Cargo.toml"
$Release = Join-Path $TargetDir "release"
$Name = "velt-$Version-$HostTriple"
$Dist = Join-Path $Repo "dist"
$Out = Join-Path $Dist $Name
if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
foreach ($d in @("bin", "lib", "std")) { New-Item -ItemType Directory -Force (Join-Path $Out $d) | Out-Null }

if (-not $SkipBuild) {
    Write-Host "building release velt + velt_rt + velt_rt_shared + velt-kit..."
    cargo build --release -p veltc -p velt_rt -p velt_rt_shared -p velt_link --manifest-path $Manifest
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

# The bundled linker (crates/velt_link/src/bundled.rs): lld.exe plus the link kit.
if (-not $NoBundledLinker) {
    if (-not $Lld) {
        $LlvmVersion = (Select-String -Path (Join-Path $PSScriptRoot "build-lld.sh") -Pattern '^VERSION=\$\{LLVM_VERSION:-(.+)\}$').Matches[0].Groups[1].Value
        $Cache = if ($env:VELT_LLD_CACHE) { $env:VELT_LLD_CACHE } else { Join-Path $env:LOCALAPPDATA "velt\lld-$LlvmVersion" }
        if (-not (Test-Path (Join-Path $Cache "lld.exe"))) {
            pwsh -NoProfile -File (Join-Path $PSScriptRoot "build-lld.ps1") -OutDir $Cache -Version $LlvmVersion
            if ($LASTEXITCODE -ne 0) { throw "building lld failed" }
        }
        $Lld = Join-Path $Cache "lld.exe"
    }
    $LldDir = Join-Path $Out "lib\velt"
    New-Item -ItemType Directory -Force $LldDir | Out-Null
    Copy-Item $Lld (Join-Path $LldDir "lld.exe")
    $Kit = Join-Path $Out "lib\targets\$HostTriple"
    & (Join-Path $Release "velt-kit.exe") build --target $HostTriple --lld (Join-Path $LldDir "lld.exe") --out $Kit
    if ($LASTEXITCODE -ne 0) { throw "building the link kit failed" }

    if (-not $SkipBuild) {
        # Relink velt.exe and the shared runtime DLL with lld-link (lld picks the flavor from its
        # file name) against the kit instead of the MSVC libraries.
        $LinkDir = Join-Path $TargetDir "lld-link"
        New-Item -ItemType Directory -Force $LinkDir | Out-Null
        Copy-Item $Lld (Join-Path $LinkDir "lld-link.exe")
        $KitLibs = Get-ChildItem (Join-Path $Kit "*.lib") | ForEach-Object { "-Clink-arg=$($_.Name)" }
        $Common = @("-Clinker=$(Join-Path $LinkDir 'lld-link.exe')", "-Clink-arg=/NODEFAULTLIB", "-Clink-arg=/LIBPATH:$Kit") + $KitLibs
        Write-Host "linking velt.exe with lld-link..."
        cargo rustc --release -p veltc --bin velt --manifest-path $Manifest -- @Common "-Clink-arg=$(Join-Path $Kit 'velt_crt.obj')"
        if ($LASTEXITCODE -ne 0) { throw "linking velt.exe with lld-link failed" }
        Write-Host "linking velt_rt_shared.dll with lld-link..."
        cargo rustc --release -p velt_rt_shared --lib --manifest-path $Manifest -- @Common "-Clink-arg=$(Join-Path $Kit 'velt_crt_dll.obj')"
        if ($LASTEXITCODE -ne 0) { throw "linking velt_rt_shared.dll with lld-link failed" }
    }
}

$Exe = Join-Path $Release "velt.exe"
$RtLib = Join-Path $Release "velt_rt.lib"
# The shared runtime debug builds link (crates/velt_rt_shared): the DLL and its import library.
$SharedRt = @((Join-Path $Release "velt_rt_shared.dll"), (Join-Path $Release "velt_rt_shared.dll.lib"))
foreach ($f in @($Exe, $RtLib) + $SharedRt) {
    if (-not (Test-Path $f -PathType Leaf)) { throw "missing build output: $f" }
}

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

Install:  get-velt.ps1 -Archive <this .zip> (an asset of every release), or
          pwsh scripts/install.ps1 -Dist <this directory> from a source checkout, or copy it anywhere.
Then add ``<prefix>\bin`` to PATH and run ``velt doctor``.

    velt run hello.vlt
    velt new app; cd app; velt run

Layout: bin/ (the velt CLI), lib/ (runtime library linked into every program),
std/ (standard library sources). Full guide: docs/tooling/platforms.md in the Velt repository.
"@ | Set-Content -Encoding utf8 (Join-Path $Out "README.md")

Copy-Item (Join-Path $Repo "LICENSE-MIT"), (Join-Path $Repo "LICENSE-APACHE"), (Join-Path $Repo "NOTICE") $Out

if (-not $NoBundledLinker) {
    $Size = { param($p) [math]::Round(((Get-ChildItem -Recurse -File $p | Measure-Object Length -Sum).Sum) / 1MB, 1) }
    Write-Host "bundled linker: lld $(& $Size (Join-Path $Out 'lib\velt')) MB, kit $(& $Size (Join-Path $Out 'lib\targets')) MB; toolchain $(& $Size $Out) MB"
}

if (-not $NoArchive) {
    $Zip = Join-Path $Dist "$Name.zip"
    if (Test-Path $Zip) { Remove-Item -Force $Zip }
    Compress-Archive -Path $Out -DestinationPath $Zip
    Write-Host "archive: $Zip"
}
Write-Host "dist:    $Out"
