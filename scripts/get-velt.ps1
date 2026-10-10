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
# A download is checked against the release's SHA256SUMS, over https only (redirects included;
# plain http only to this machine). PowerShell cannot check SHA256SUMS's Ed25519 signature; the
# installed launcher checks the signature of everything it downloads later.
param(
    [string]$Version = $env:VELT_INSTALL_VERSION,
    [string]$Prefix = $env:VELT_INSTALL_PREFIX,
    [string]$Archive = "",
    [switch]$Default,
    [switch]$Force,
    [switch]$NoModifyPath
)

# In a script block, so `irm | iex` leaves no variables or preferences in the caller's session
# (only `$env:Path`, which it is meant to update).
& {
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

# https, or http to this machine (tests, local mirrors): the launcher's rule.
function Test-AllowedUrl([Uri]$Uri) {
    $Uri.Scheme -eq "https" -or ($Uri.Scheme -eq "http" -and $Uri.IsLoopback)
}
if (-not (Test-AllowedUrl ([Uri]$BaseUrl))) {
    Fail "VELT_INSTALL_BASE_URL must be an https:// URL (or http:// to this machine): $BaseUrl"
}

# Download $Url to $File, refusing a redirect to anything but https (or loopback http).
function Get-File([string]$Url, [string]$File) {
    $Response = Invoke-WebRequest -UseBasicParsing $Url -OutFile $File -PassThru
    $Final = if ($Response.BaseResponse.ResponseUri) {
        $Response.BaseResponse.ResponseUri                  # Windows PowerShell 5.1
    } else {
        $Response.BaseResponse.RequestMessage.RequestUri    # PowerShell 7
    }
    if ($Final -and -not (Test-AllowedUrl $Final)) {
        Remove-Item -Force $File -ErrorAction SilentlyContinue
        Fail "$Url redirected to $Final, which is not https: refusing it"
    }
}

# `velt-launcher <version>` from `<launcher> toolchain --version`, else $null: an older velt.exe
# there (the compiler of an install before the launcher) or anything else is not a launcher.
function Get-LauncherVersion([string]$Exe) {
    try {
        $Out = & $Exe toolchain --version 2>$null
        if ($LASTEXITCODE -ne 0) { return $null }
    } catch {
        return $null
    }
    foreach ($Line in @($Out)) {
        if ("$Line" -match '^velt-launcher (\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?)$') { return $Matches[1] }
    }
    $null
}

# -1, 0 or 1: semver order of release versions, a pre-release before its release.
function Compare-Version([string]$A, [string]$B) {
    $CoreA = [version]($A -split "-", 2)[0]
    $CoreB = [version]($B -split "-", 2)[0]
    if ($CoreA -ne $CoreB) { return $CoreA.CompareTo($CoreB) }
    $PreA = if ($A.Contains("-")) { ($A -split "-", 2)[1] } else { $null }
    $PreB = if ($B.Contains("-")) { ($B -split "-", 2)[1] } else { $null }
    if ($PreA -eq $PreB) { return 0 }
    if (-not $PreA) { return 1 }
    if (-not $PreB) { return -1 }
    [string]::CompareOrdinal($PreA, $PreB)
}

# `velt <version> (<commit> <triple>)` → <version>.
function Get-ToolchainVersion([string]$Exe) {
    try {
        $Out = & $Exe --version 2>$null
        if ($LASTEXITCODE -ne 0) { return $null }
    } catch {
        return $null
    }
    foreach ($Line in @($Out)) {
        if ("$Line" -match '^velt (\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?) ') { return $Matches[1] }
    }
    $null
}

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("velt-install-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force $Tmp | Out-Null
$Staging = $null
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
            Get-File "$Url/$Name.zip" $Archive
        } catch {
            Fail "download failed: $Url/$Name.zip (is $Version a release with a $Target build?) $_"
        }
        $Sums = Join-Path $Tmp "SHA256SUMS"
        try {
            Get-File "$Url/SHA256SUMS" $Sums
        } catch {
            Fail "download failed: $Url/SHA256SUMS $_"
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
    $Version = Get-ToolchainVersion (Join-Path $Dist "bin\velt.exe")
    if (-not $Version) { Fail "the velt.exe in $Archive does not run, or does not say its version" }

    New-Item -ItemType Directory -Force $Prefix | Out-Null
    $Root = (Resolve-Path $Prefix).Path
    $Bin = Join-Path $Root "bin"
    $Toolchains = Join-Path $Root "toolchains"

    # A single toolchain an installer before the launcher put into the root: its bin\velt.exe is
    # the compiler. It moves into toolchains\<its version>, where the launcher runs it; the new
    # version becomes the default (there is no default file yet).
    if ((Test-Path (Join-Path $Root "std")) -and -not (Test-Path $Toolchains)) {
        $OldVelt = Join-Path $Root "bin\velt.exe"
        $OldVersion = if (Test-Path $OldVelt) { Get-ToolchainVersion $OldVelt } else { $null }
        if (-not $OldVersion) {
            Fail "$Root holds an earlier install whose bin\velt.exe does not say its version; remove $Root\bin\velt.exe, $Root\lib and $Root\std, then run this again"
        }
        $Moved = Join-Path $Toolchains $OldVersion
        Write-Host "moving the earlier install of velt $OldVersion into $Moved"
        New-Item -ItemType Directory -Force (Join-Path $Moved "bin") | Out-Null
        Move-Item $OldVelt (Join-Path $Moved "bin\velt.exe")
        foreach ($d in @("lib", "std", "share", "README.md", "LICENSE-MIT", "LICENSE-APACHE", "NOTICE")) {
            if (Test-Path (Join-Path $Root $d)) { Move-Item (Join-Path $Root $d) (Join-Path $Moved $d) }
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
        $Staging = $null
        Write-Host "installed velt $Version into $Dest"
    }

    # The launcher: replaced unless the one installed belongs to a newer velt. A running
    # velt.exe can be renamed but not overwritten, so the old one is renamed aside first.
    $Launcher = Join-Path $Bin "velt.exe"
    $Current = if (Test-Path $Launcher) { Get-LauncherVersion $Launcher } else { $null }
    if (-not $Current -or (Compare-Version $Version $Current) -ge 0) {
        if (Test-Path $Launcher) {
            $Aside = Join-Path $Bin (".velt.exe.old-" + [guid]::NewGuid().ToString("N"))
            Rename-Item $Launcher $Aside
        }
        Copy-Item $NewLauncher $Launcher
    }
    Get-ChildItem $Bin -Filter ".velt.exe.old-*" -Force -ErrorAction SilentlyContinue |
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
    # A failed install leaves no partial toolchain behind.
    if ($Staging) { Remove-Item -Recurse -Force $Staging -ErrorAction SilentlyContinue }
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

# The toolchain links programs with its bundled linker: no Visual Studio needed (`velt doctor`
# says what is missing, if anything).
if (-not $OnPath -and $NoModifyPath) {
    Write-Host ""
    Write-Host "Add Velt to your PATH for this session:"
    Write-Host "  `$env:Path = '$Bin;' + `$env:Path"
}
Write-Host ""
Write-Host "Then check the installation with:  velt doctor"
}
