# Windows counterpart of scripts/smoke-clean.sh: an installed toolchain on a machine without
# Visual Studio (release.yml runs it in a Windows Server Core container: no Build Tools, no
# Windows SDK, no Visual C++ runtime) builds and runs hello and examples/http_hello.vlt in debug
# and release mode with the bundled linker, and `velt doctor` reports it. Runs under Windows
# PowerShell 5.1 (what Server Core has).
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
    Velt build @Flags (Join-Path $Repo "tests\golden\m1\hello.vlt") -o $Hello
    $Out = & $Hello
    if ($LASTEXITCODE -ne 0 -or $Out -ne "Hello, Velt!") { throw "$Label hello printed '$Out' (exit $LASTEXITCODE)" }
    $Http = Join-Path $Work "http.exe"
    Velt build @Flags (Join-Path $Repo "examples\http_hello.vlt") -o $Http
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
Step "debug build (shared runtime)"
RunBoth "debug" @()
Step "release build"
RunBoth "release" @("--release")
