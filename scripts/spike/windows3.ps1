# TEMPORARY spike (#803), round 3: why do throws/finally crash with the shim? Control: link.exe.
param([string]$Out = "spike-out")
$ErrorActionPreference = "Continue"
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
function Section($t) { Write-Host "`n===== $t =====" }
cargo build --release -p veltc -p velt_rt 2>&1 | Select-Object -Last 1
$velt = (Resolve-Path "target/release/velt.exe").Path
$rt = (Resolve-Path "target/release/velt_rt.lib").Path
$sysroot = (rustc --print sysroot).Trim()
$lld = Join-Path $sysroot "lib/rustlib/x86_64-pc-windows-msvc/bin/rust-lld.exe"
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -property installationPath
$dumpbin = Get-ChildItem "$vs\VC\Tools\MSVC" -Recurse -Filter dumpbin.exe | Where-Object { $_.FullName -match 'Hostx64\\x64' } | Select-Object -First 1 -ExpandProperty FullName
$cdb = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\Debuggers\x64\cdb.exe" -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
Write-Host "cdb: $cdb"
$kit = "$Out/kit"; New-Item -ItemType Directory -Force $kit | Out-Null
rustc --edition 2021 --crate-type lib --crate-name velt_crt --emit "obj=$kit/velt_crt.obj" --target x86_64-pc-windows-msvc -C panic=abort -C opt-level=2 -C overflow-checks=off -C debug-assertions=off crates/velt_link/kit/crt/windows_x86_64.rs
$defs = @{ kernel32="kernel32"; ntdll="ntdll"; advapi32="advapi32"; ws2_32="ws2_32"; bcrypt="bcrypt"; userenv="userenv"; dbghelp="dbghelp"; secur32="secur32"; psapi="psapi"; shell32="shell32"; user32="user32"; ucrtbase="ucrt" }
foreach ($d in $defs.Keys) {
  $names = @()
  foreach ($line in (& $dumpbin /nologo /exports "$env:WINDIR\System32\$d.dll")) {
    if ($line -match '^\s+\d+\s+[0-9A-F]+\s+[0-9A-F]{8}\s+(\S+)') { $names += $matches[1] }
    elseif ($line -match '^\s+\d+\s+[0-9A-F]+\s+(\S+)\s+\(forwarded') { $names += $matches[1] }
  }
  Set-Content -Path "$kit/$($defs[$d]).def" -Value ("LIBRARY $d.dll`r`nEXPORTS`r`n" + (($names | ForEach-Object { "  $_" }) -join "`r`n"))
  & $lld -flavor link /lib /nologo /machine:x64 "/def:$kit/$($defs[$d]).def" "/out:$kit/$($defs[$d]).lib" | Out-Null
}
Set-Content "$kit/synchronization.def" "LIBRARY api-ms-win-core-synch-l1-2-0.dll`r`nEXPORTS`r`n  WaitOnAddress`r`n  WakeByAddressSingle`r`n  WakeByAddressAll"
& $lld -flavor link /lib /nologo /machine:x64 "/def:$kit/synchronization.def" "/out:$kit/synchronization.lib" | Out-Null
$sys = "kernel32.lib","ntdll.lib","advapi32.lib","ws2_32.lib","bcrypt.lib","userenv.lib","dbghelp.lib","secur32.lib","psapi.lib","shell32.lib","user32.lib","synchronization.lib","ucrt.lib"

$progs = [ordered]@{ throws="tests/golden/bugs/lower_ctor_throws_after_sharing_this.vlt"; finally="tests/golden/lang/finally_after_return_keeps_values.vlt"; hello="tests/golden/m1/hello.vlt" }
foreach ($p in $progs.Keys) {
  & $velt build --emit obj $progs[$p] -o "$Out/$p.obj" 2>&1 | Select-Object -Last 3
  # control: velt's normal link (link.exe + MSVC libs), static runtime
  $env:VELT_RT_LINK = "static"
  & $velt build $progs[$p] -o "$Out/$p-msvc.exe" 2>&1 | Select-Object -Last 3
  Remove-Item Env:VELT_RT_LINK
  & $lld -flavor link /nologo /NODEFAULTLIB /SUBSYSTEM:CONSOLE /INCREMENTAL:NO /DEBUG "/LIBPATH:$kit" "/OUT:$Out/$p-lld.exe" "$kit/velt_crt.obj" "$Out/$p.obj" $rt $sys 2>&1 | Select-Object -First 5
  # lld-link with the MSVC libs (isolates the linker from the shim)
  & $lld -flavor link /nologo /SUBSYSTEM:CONSOLE /INCREMENTAL:NO /DEBUG "/OUT:$Out/$p-lldmsvc.exe" "$Out/$p.obj" $rt kernel32.lib ntdll.lib advapi32.lib ws2_32.lib bcrypt.lib userenv.lib dbghelp.lib secur32.lib psapi.lib shell32.lib user32.lib synchronization.lib msvcrt.lib "/LIBPATH:$((Get-ChildItem "$vs\VC\Tools\MSVC" | Select-Object -Last 1).FullName)\lib\x64" "/LIBPATH:${env:ProgramFiles(x86)}\Windows Kits\10\Lib\$((Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\Lib" | Select-Object -Last 1).Name)\um\x64" "/LIBPATH:${env:ProgramFiles(x86)}\Windows Kits\10\Lib\$((Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\Lib" | Select-Object -Last 1).Name)\ucrt\x64" 2>&1 | Select-Object -First 5
  foreach ($v in "msvc","lldmsvc","lld") {
    Section "$p-$v"
    if (Test-Path "$Out/$p-$v.exe") { cmd /c "`"$Out/$p-$v.exe`" 2>&1"; Write-Host "exit $LASTEXITCODE" } else { Write-Host "(not linked)" }
  }
}
Section "tls directories"
foreach ($v in "msvc","lld") { Write-Host "-- $v"; & $dumpbin /nologo /headers "$Out/finally-$v.exe" | Select-String -Pattern "Thread Storage|\.tls|TLS" | ForEach-Object { $_.Line.Trim() }; & $dumpbin /nologo /tls "$Out/finally-$v.exe" 2>$null | Select-Object -First 30 }
Section "cdb throws-lld"
if ($cdb) {
  & $cdb -g -G -lines -c ".symfix+ C:\symcache; .reload; .ecxr; kn 30; r; u @rip L8; q" "$Out/throws-lld.exe" 2>&1 | Select-Object -Last 70
  Section "cdb finally-lld"
  & $cdb -g -G -lines -c ".ecxr; kn 30; r; q" "$Out/finally-lld.exe" 2>&1 | Select-Object -Last 50
}
