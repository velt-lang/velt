# Windows counterpart of scripts/smoke-clean.sh: an installed toolchain on a machine without
# Visual Studio (release.yml runs it in a Windows Server Core container: no Build Tools, no
# Windows SDK, no Visual C++ runtime) builds and runs hello and examples/http_hello.velt in debug
# and release mode with the bundled linker, and `velt doctor` reports it; a debug build of
# scripts\smoke-prebuilt.velt links the toolchain's prebuilt hook objects (lib\hooks\); hello also builds for
# wasm32-unknown-unknown; tests\golden\std\fs_dirents.velt lists a directory with its entries'
# types. Runs under Windows PowerShell 5.1 (what Server Core has).
#
# Usage: powershell -File scripts/smoke-clean.ps1 <path to velt.exe>
param([Parameter(Mandatory)][string]$Velt)
$ErrorActionPreference = "Stop"
$Repo = Split-Path -Parent $PSScriptRoot
$Work = Join-Path ([IO.Path]::GetTempPath()) "velt-smoke-clean"
if (Test-Path $Work) { Remove-Item -Recurse -Force $Work }
New-Item -ItemType Directory -Force $Work | Out-Null
$env:VELT_LINKER = "bundled"

function Step($t) { Write-Host "`n== $t" }
function Velt { & $Velt @args; if ($LASTEXITCODE -ne 0) { throw "velt $args failed ($LASTEXITCODE)" } }

function RunBoth($Label, [string[]]$Flags) {
    $Hello = Join-Path $Work "hello.exe"
    Velt build @Flags (Join-Path $Repo "tests\golden\m1\hello.velt") -o $Hello
    $Out = & $Hello
    if ($LASTEXITCODE -ne 0 -or $Out -ne "Hello, Velt!") { throw "$Label hello printed '$Out' (exit $LASTEXITCODE)" }
    $Http = Join-Path $Work "http.exe"
    Velt build @Flags (Join-Path $Repo "examples\http_hello.velt") -o $Http
    $env:VELT_HELLO_SECONDS = "3"
    $Server = Start-Process -FilePath $Http -PassThru -NoNewWindow -RedirectStandardOutput (Join-Path $Work "http.out")
    Remove-Item Env:VELT_HELLO_SECONDS
    $Body = $null
    for ($i = 0; $i -lt 50 -and -not $Body; $i++) {
        try { $Body = (Invoke-WebRequest -UseBasicParsing http://127.0.0.1:8080/).Content } catch { Start-Sleep -Milliseconds 100 }
    }
    $Server.WaitForExit()
    if ($Body -ne "Hello, World!") { throw "$Label http_hello answered '$Body'" }
    Write-Host "ok: $Label"
}

Step "no Visual Studio"
foreach ($f in @("$env:WINDIR\System32\vcruntime140.dll", "${env:ProgramFiles(x86)}\Microsoft Visual Studio")) {
    if (Test-Path $f) { Write-Host "note: $f exists; `$VELT_LINKER=bundled keeps link.exe unused" }
}
Step "velt doctor"
$Doctor = & $Velt doctor 2>&1 | Out-String
Write-Host $Doctor
if ($Doctor -notmatch "linker\s+bundled") { throw "velt doctor does not report the bundled linker" }
# The toolchain's own objects of the prebuilt std hook modules (lib\hooks\<target>\), with the
# build cache off so nothing else could hold them: a debug build links them, also where no clang
# could have compiled them.
Step "prebuilt hook modules (shipped objects)"
$Prebuilt = Join-Path $Work "prebuilt.exe"
$env:VELT_BUILD_CACHE = "off"
$env:VELT_STD_OBJECT_STATS = "1"
# Windows PowerShell turns a native command's redirected stderr lines into error records, which
# "Stop" would throw on: read them as text instead.
$ErrorActionPreference = "Continue"
$Stats = & $Velt build (Join-Path $Repo "scripts\smoke-prebuilt.velt") -o $Prebuilt 2>&1 | ForEach-Object { "$_" } | Out-String
$Code = $LASTEXITCODE
$ErrorActionPreference = "Stop"
Remove-Item Env:VELT_BUILD_CACHE, Env:VELT_STD_OBJECT_STATS
if ($Code -ne 0) { throw "velt build smoke-prebuilt.velt failed ($Code): $Stats" }
foreach ($Module in @("number_digits", "number_parse", "weak")) {
    if ($Stats -notmatch "prebuilt hooks: std/lang/hooks/${Module}: object found \(shipped\)") {
        throw "the build did not link the toolchain's object of lang/hooks/${Module}: $Stats"
    }
}
$Out = & $Prebuilt
if ($LASTEXITCODE -ne 0 -or $Out -ne "1234.57 356 7") { throw "smoke-prebuilt printed '$Out' (exit $LASTEXITCODE)" }
Write-Host "ok: shipped objects linked"
Step "directory entries"
$Dirents = Join-Path $Work "dirents.exe"
Velt build (Join-Path $Repo "tests\golden\std\fs_dirents.velt") -o $Dirents
Push-Location $Work
$Out = (& $Dirents | Out-String) -replace "`r`n", "`n"
$Code = $LASTEXITCODE
Pop-Location
$Want = ([IO.File]::ReadAllText((Join-Path $Repo "tests\golden\std\fs_dirents.out"))) -replace "`r`n", "`n"
if ($Code -ne 0 -or $Out -ne $Want) { throw "fs_dirents printed '$Out' (exit $Code), not tests\golden\std\fs_dirents.out" }
Write-Host "ok: directory entries"
Step "debug build (shared runtime)"
RunBoth "debug" @()
Step "release build"
RunBoth "release" @("--release")
# With no Rust or LLVM on the machine: the toolchain's WebAssembly runtime, compiled through the
# pinned zig, which velt downloads on first use (#2587).
Step "WebAssembly (wasm32-unknown-unknown, release)"
$Wasm = Join-Path $Work "hello.wasm"
Velt build --release --target wasm32-unknown-unknown (Join-Path $Repo "tests\golden\m1\hello.velt") -o $Wasm
$Magic = [IO.File]::ReadAllBytes($Wasm)[0..3]
if (($Magic -join ",") -ne "0,97,115,109") { throw "hello.wasm is no WebAssembly module" }
Write-Host "ok: wasm32-unknown-unknown"
