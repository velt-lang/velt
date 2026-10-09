# TEMPORARY spike (#803): what does lld-link need to link a Velt program without the MSVC/SDK libs?
param([string]$Out = "spike-out")
$ErrorActionPreference = "Continue"
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
function Section($t) { Write-Host "`n===== $t =====" }

Section "build"
cargo build --release -p veltc -p velt_rt -p velt_rt_shared 2>&1 | Select-Object -Last 3
$velt = "target/release/velt.exe"
$rt = (Resolve-Path "target/release/velt_rt.lib").Path

Section "rust-lld"
$sysroot = (rustc --print sysroot).Trim()
$lld = Join-Path $sysroot "lib/rustlib/x86_64-pc-windows-msvc/bin/rust-lld.exe"
Get-Item $lld | Select-Object FullName, Length | Format-List
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -property installationPath
$dumpbin = Get-ChildItem "$vs\VC\Tools\MSVC" -Recurse -Filter dumpbin.exe | Where-Object { $_.FullName -match 'Hostx64\\x64' } | Select-Object -First 1 -ExpandProperty FullName
& $dumpbin /nologo /dependents $lld
& $lld -flavor link --version

Section "objects"
& $velt build --emit obj tests/golden/m1/hello.vlt -o "$Out/hello.obj"
& $velt build --emit obj examples/http_hello.vlt -o "$Out/http.obj"
Get-ChildItem $Out

Section "import libs from System32"
$libs = "$Out/libs"; New-Item -ItemType Directory -Force $libs | Out-Null
Set-Content "$libs/api-ms-win-core-synch-l1-2-0.def" "LIBRARY api-ms-win-core-synch-l1-2-0.dll`r`nEXPORTS`r`n  WaitOnAddress`r`n  WakeByAddressSingle`r`n  WakeByAddressAll"
& $lld -flavor link /lib /nologo /machine:x64 "/def:$libs/api-ms-win-core-synch-l1-2-0.def" "/out:$libs/api-ms-win-core-synch-l1-2-0.lib"
$dlls = "kernel32","ntdll","advapi32","ws2_32","bcrypt","userenv","dbghelp","secur32","psapi","shell32","user32","ucrtbase","msvcrt","ole32","oleaut32","crypt32","ncrypt","iphlpapi"
foreach ($d in $dlls) {
  $path = "$env:WINDIR\System32\$d.dll"
  if (-not (Test-Path $path)) { Write-Host "missing $path"; continue }
  $names = @()
  foreach ($line in (& $dumpbin /nologo /exports $path)) {
    if ($line -match '^\s+\d+\s+[0-9A-F]+\s+[0-9A-F]{8}\s+(\S+)') { $names += $matches[1] }
    elseif ($line -match '^\s+\d+\s+[0-9A-F]+\s+(\S+)\s+\(forwarded') { $names += $matches[1] }
  }
  $def = "LIBRARY $d.dll`r`nEXPORTS`r`n" + (($names | ForEach-Object { "  $_" }) -join "`r`n")
  $libname = if ($d -eq "msvcrt") { "msvcrt_dll" } else { $d }
  Set-Content -Path "$libs/$libname.def" -Value $def
  & $lld -flavor link /lib /nologo /machine:x64 "/def:$libs/$libname.def" "/out:$libs/$libname.lib" 2>&1 | Select-Object -First 3
  Write-Host "$d : $($names.Count) exports"
}

Section "directives in velt_rt.lib (DEFAULTLIB)"
& $dumpbin /nologo /directives $rt | Select-String -Pattern '/DEFAULTLIB' | ForEach-Object { $_.Line.Trim() } | Sort-Object -Unique

$sys = "kernel32.lib","ntdll.lib","advapi32.lib","ws2_32.lib","bcrypt.lib","userenv.lib","dbghelp.lib","secur32.lib","psapi.lib","shell32.lib","user32.lib","api-ms-win-core-synch-l1-2-0.lib"
foreach ($p in "hello","http") {
  Section "link $p : NODEFAULTLIB, system DLL libs only (no CRT)"
  $log = "$Out/$p-nocrt.log"
  & $lld -flavor link /nologo /NODEFAULTLIB /SUBSYSTEM:CONSOLE "/LIBPATH:$libs" "/OUT:$Out/$p-a.exe" "$Out/$p.obj" $rt $sys "/errorlimit:0" *> $log
  Write-Host "exit $LASTEXITCODE"
  Get-Content $log | Select-String "undefined symbol" | ForEach-Object { ($_.Line -replace '.*undefined symbol: ','').Trim() } | Sort-Object -Unique | Tee-Object "$Out/$p-undef-nocrt.txt"
  Get-Content $log | Select-String -NotMatch "undefined symbol|>>> referenced" | Select-Object -First 20
  Section "link $p : + ucrtbase + msvcrt.dll"
  $log = "$Out/$p-ucrt.log"
  & $lld -flavor link /nologo /NODEFAULTLIB /SUBSYSTEM:CONSOLE "/LIBPATH:$libs" "/OUT:$Out/$p-b.exe" "$Out/$p.obj" $rt $sys ucrtbase.lib msvcrt_dll.lib "/errorlimit:0" *> $log
  Write-Host "exit $LASTEXITCODE"
  Get-Content $log | Select-String "undefined symbol" | ForEach-Object { ($_.Line -replace '.*undefined symbol: ','').Trim() } | Sort-Object -Unique | Tee-Object "$Out/$p-undef-ucrt.txt"
  Get-Content $log | Select-String -NotMatch "undefined symbol|>>> referenced" | Select-Object -First 20
}
Section "run b"
if (Test-Path "$Out/hello-b.exe") { & "$Out/hello-b.exe"; Write-Host "exit $LASTEXITCODE" }
