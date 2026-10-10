# Download and install a released Velt toolchain on Windows (PowerShell 5.1 or 7).
#
#   irm https://github.com/velt-lang/velt/releases/latest/download/get-velt.ps1 | iex
#
# Versions install side by side (#948, docs/tooling/platforms.md): the toolchain goes into
# <root>\toolchains\<version>\, and the launcher into <root>\bin\velt.exe, the one directory on
# PATH. It runs the version each package pins (`velt` in package.vlt), else the default. Running
# this again with another version adds it beside the others.
#
# Parameters when run as a file (environment variable in parentheses, also honored by `| iex`):
#   -Version <v>     the release to install, e.g. 0.1.1 (VELT_INSTALL_VERSION); default: the release
#                    this script was published with, or the latest release
#   -Prefix <dir>    the root (VELT_INSTALL_PREFIX); default: %LOCALAPPDATA%\velt
#   -Default         make this version the default (the first one installed always is)
#   -Force           reinstall a version that is already installed
#   -Archive <file>  install a downloaded velt-<version>-<target>.zip instead of downloading
#   -NoModifyPath    do not add <root>\bin to the user PATH
# VELT_INSTALL_BASE_URL replaces https://github.com/velt-lang/velt (forks, mirrors, tests).
#
# A download is checked against the release's SHA256SUMS. PowerShell cannot check its Ed25519
# signature; the installed launcher checks the signature of everything it downloads later.
param(
    [string]$Version = $env:VELT_INSTALL_VERSION,
    [string]$Prefix = $env:VELT_INSTALL_PREFIX,
    [string]$Archive = "",
    [switch]$Default,
    [switch]$Force,
    [switch]$NoModifyPath
)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"   # Invoke-WebRequest is very slow with a progress bar

# The release workflow replaces this with the version it publishes.
$PublishedVersion = "@VELT_RELEASE_VERSION@"

$BaseUrl = if ($env:VELT_INSTALL_BASE_URL) { $env:VELT_INSTALL_BASE_URL } else { "https://github.com/velt-lang/velt" }
if (-not $Prefix) { $Prefix = Join-Path $env:LOCALAPPDATA "velt" }

function Fail([string]$Message) {
    Write-Host "error: $Message" -ForegroundColor Red
    throw "velt install failed: $Message"
}

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("velt-install-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force $Tmp | Out-Null
try {
    if ($Archive) {
        if (-not (Test-Path $Archive -PathType Leaf)) { Fail "no such archive: $Archive" }
        $Archive = (Resolve-Path $Archive).Path
        Write-Host "installing Velt from $Archive"
    } else {
        # PowerShell 5.1 does not offer TLS 1.2 by default.
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

        $Arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
        switch ($Arch) {
            "AMD64" { }
            "ARM64" { Write-Host "note: there is no Windows arm64 build yet; installing the x64 build (runs under emulation)" }
            default { Fail "no prebuilt Velt for Windows $Arch; build it from source (README.md)" }
        }
        $Target = "x86_64-pc-windows-msvc"

        if (-not $Version -and -not $PublishedVersion.StartsWith("@")) { $Version = $PublishedVersion }
        if (-not $Version) {
            $Api = $BaseUrl -replace "^https://github.com/", "https://api.github.com/repos/"
            try {
                $Version = (Invoke-RestMethod -UseBasicParsing "$Api/releases/latest").tag_name
            } catch {
                Fail "could not determine the latest release of $BaseUrl (pass -Version or set VELT_INSTALL_VERSION)"
            }
        }
        $Version = $Version -replace "^v", ""
        $Name = "velt-$Version-$Target"
        $Url = "$BaseUrl/releases/download/v$Version"
        $Archive = Join-Path $Tmp "$Name.zip"
        Write-Host "downloading Velt $Version for $Target"
        try {
            Invoke-WebRequest -UseBasicParsing "$Url/$Name.zip" -OutFile $Archive
        } catch {
            Fail "download failed: $Url/$Name.zip (is $Version a release with a $Target build?)"
        }
        $Sums = Join-Path $Tmp "SHA256SUMS"
        try {
            Invoke-WebRequest -UseBasicParsing "$Url/SHA256SUMS" -OutFile $Sums
        } catch {
            Fail "download failed: $Url/SHA256SUMS"
        }
        $Expected = $null
        foreach ($Line in Get-Content $Sums) {
            $Parts = $Line -split "\s+\*?", 2
            if ($Parts.Count -eq 2 -and $Parts[1].Trim() -eq "$Name.zip") { $Expected = $Parts[0] }
        }
        if (-not $Expected) { Fail "SHA256SUMS has no entry for $Name.zip" }
        $Actual = (Get-FileHash -Algorithm SHA256 $Archive).Hash
        if ($Actual -ne $Expected) { Fail "checksum mismatch for $Name.zip (expected $Expected, got $Actual)" }
    }

    # Unpack.
    $Unpacked = Join-Path $Tmp "unpacked"
    Expand-Archive -Path $Archive -DestinationPath $Unpacked -Force
    $Dist = Get-ChildItem -Directory $Unpacked |
        Where-Object { Test-Path (Join-Path $_.FullName "bin\velt.exe") -PathType Leaf } |
        Select-Object -First 1
    if (-not $Dist) { Fail "$Archive is not a Velt release archive (no *\bin\velt.exe inside)" }
    $Dist = $Dist.FullName
    $NewLauncher = Join-Path $Dist "bin\velt-launcher.exe"
    if (-not (Test-Path $NewLauncher -PathType Leaf)) {
        Fail "$Archive has no launcher (bin\velt-launcher.exe): velt 0.1.0 predates it; this installer installs later releases"
    }
    # `velt <version> (<commit> <triple>)`
    $Installed = & (Join-Path $Dist "bin\velt.exe") --version
    if ($LASTEXITCODE -ne 0) { Fail "the velt.exe in $Archive does not run" }
    $Version = ($Installed -split "\s+")[1]
    if (-not $Version) { Fail "cannot read the version of the velt.exe in $Archive" }

    New-Item -ItemType Directory -Force $Prefix | Out-Null
    $Root = (Resolve-Path $Prefix).Path
    $Bin = Join-Path $Root "bin"
    $Toolchains = Join-Path $Root "toolchains"

    # A single toolchain installed into the root by an installer from before the launcher: its
    # bin\velt.exe is the compiler, which the launcher replaces. Not moved: removed.
    if ((Test-Path (Join-Path $Root "std")) -and -not (Test-Path $Toolchains)) {
        Write-Host "replacing the earlier single-toolchain install in $Root"
        foreach ($d in @("lib", "std")) {
            Remove-Item -Recurse -Force (Join-Path $Root $d) -ErrorAction SilentlyContinue
        }
        foreach ($f in @("README.md", "LICENSE-MIT", "LICENSE-APACHE", "NOTICE")) {
            Remove-Item -Force (Join-Path $Root $f) -ErrorAction SilentlyContinue
        }
    }
    New-Item -ItemType Directory -Force $Toolchains, $Bin | Out-Null

    # The toolchain, into place whole: a staging copy, renamed (an older copy is renamed aside).
    $Dest = Join-Path $Toolchains $Version
    if ((Test-Path (Join-Path $Dest "bin\velt.exe")) -and -not $Force) {
        Write-Host "velt $Version is already installed in $Dest (-Force reinstalls it)"
    } else {
        $Staging = Join-Path $Toolchains ".$Version.$PID"
        if (Test-Path $Staging) { Remove-Item -Recurse -Force $Staging }
        Copy-Item -Recurse $Dist $Staging
        if (Test-Path $Dest) {
            $Old = Join-Path $Toolchains ".$Version.old.$PID"
            try { Rename-Item $Dest $Old } catch { Fail "cannot replace $Dest (in use?)" }
            Remove-Item -Recurse -Force $Old -ErrorAction SilentlyContinue
        }
        Rename-Item $Staging $Dest
        Write-Host "installed $Installed into $Dest"
    }

    # The launcher: replaced unless the one installed belongs to a newer velt. A running
    # velt.exe can be renamed but not overwritten, so the old one is renamed aside first.
    $Launcher = Join-Path $Bin "velt.exe"
    $Current = $null
    if (Test-Path $Launcher) {
        $Out = & $Launcher toolchain --version 2>$null
        if ($LASTEXITCODE -eq 0 -and $Out) { $Current = ($Out -split "\s+")[1] }
    }
    $Release = { param($v) [version](($v -split "-")[0]) }
    if (-not $Current -or (& $Release $Version) -ge (& $Release $Current)) {
        if (Test-Path $Launcher) {
            $Aside = Join-Path $Bin (".velt.exe.old-" + [guid]::NewGuid().ToString("N"))
            Rename-Item $Launcher $Aside
            Remove-Item -Force $Aside -ErrorAction SilentlyContinue
        }
        Copy-Item $NewLauncher $Launcher
    }
    Get-ChildItem $Bin -Filter ".velt.exe.old-*" -ErrorAction SilentlyContinue |
        Remove-Item -Force -ErrorAction SilentlyContinue

    $DefaultFile = Join-Path $Root "default"
    $HasDefault = (Test-Path $DefaultFile) -and ((Get-Content -Raw $DefaultFile).Trim())
    if ($Default -or -not $HasDefault) {
        [IO.File]::WriteAllText($DefaultFile, "$Version`n")
        Write-Host "velt $Version is the default (packages that pin another version run that one)"
    }
    $env:VELT_TOOLCHAIN = $Version
    try {
        & $Launcher --version | Out-Null
        if ($LASTEXITCODE -ne 0) { Fail "the launcher in $Bin does not run velt $Version" }
    } finally {
        Remove-Item Env:VELT_TOOLCHAIN -ErrorAction SilentlyContinue
    }
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}

# PATH: the user's Path in the registry, keeping its value kind (REG_EXPAND_SZ entries such as
# %USERPROFILE%\... must stay unexpanded).
$OnPath = ($env:Path -split ";") -contains $Bin
if (-not $NoModifyPath) {
    $Key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment", $true)
    $Old = $Key.GetValue("Path", "", [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    if (($Old -split ";") -notcontains $Bin) {
        $Kind = if ($Old) { $Key.GetValueKind("Path") } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
        $New = if ($Old) { $Old.TrimEnd(";") + ";" + $Bin } else { $Bin }
        $Key.SetValue("Path", $New, $Kind)
        # Setting any user variable through .NET broadcasts WM_SETTINGCHANGE, so new terminals
        # started from Explorer see the new Path.
        [Environment]::SetEnvironmentVariable("VELT_INSTALL_REFRESH", "1", "User")
        [Environment]::SetEnvironmentVariable("VELT_INSTALL_REFRESH", [NullString]::Value, "User")
        Write-Host "  added $Bin to the user PATH"
    }
    $Key.Close()
    if (-not $OnPath) { $env:Path = "$Bin;$env:Path" }
}

Write-Host ""
Write-Host "Velt links programs with the MSVC linker: install the Build Tools for Visual Studio"
Write-Host "(""Desktop development with C++"") if you have not; ``velt doctor`` checks for it."
if (-not $OnPath -and $NoModifyPath) {
    Write-Host ""
    Write-Host "Add Velt to your PATH for this session:"
    Write-Host "  `$env:Path = '$Bin;' + `$env:Path"
}
Write-Host ""
Write-Host "Then check the installation with:  velt doctor"
