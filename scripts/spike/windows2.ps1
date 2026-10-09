# TEMPORARY spike (#803), round 2: lld-link + generated import libs + velt_crt shim, no MSVC libs.
param([string]$Out = "spike-out")
$ErrorActionPreference = "Continue"
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
function Section($t) { Write-Host "`n===== $t =====" }
Section "build"
cargo build --release -p veltc -p velt_rt -p velt_rt_shared 2>&1 | Select-Object -Last 2
$velt = (Resolve-Path "target/release/velt.exe").Path
$rt = (Resolve-Path "target/release/velt_rt.lib").Path
$sysroot = (rustc --print sysroot).Trim()
$lld = Join-Path $sysroot "lib/rustlib/x86_64-pc-windows-msvc/bin/rust-lld.exe"
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -property installationPath
$dumpbin = Get-ChildItem "$vs\VC\Tools\MSVC" -Recurse -Filter dumpbin.exe | Where-Object { $_.FullName -match 'Hostx64\\x64' } | Select-Object -First 1 -ExpandProperty FullName

Section "shim"
$kit = "$Out/kit"; New-Item -ItemType Directory -Force $kit | Out-Null
rustc --edition 2021 --crate-type lib --crate-name velt_crt --emit "obj=$kit/velt_crt.obj" --target x86_64-pc-windows-msvc -C panic=abort -C opt-level=2 -C overflow-checks=off -C debug-assertions=off crates/velt_link/kit/crt/windows_x86_64.rs
rustc --edition 2021 --crate-type lib --crate-name velt_crt --emit "obj=$kit/velt_crt_dll.obj" --target x86_64-pc-windows-msvc -C panic=abort -C opt-level=2 -C overflow-checks=off -C debug-assertions=off --cfg velt_crt_dll crates/velt_link/kit/crt/windows_x86_64.rs
Get-ChildItem $kit

Section "import libs"
$defs = "$Out/defs"; New-Item -ItemType Directory -Force $defs | Out-Null
$map = [ordered]@{ kernel32="kernel32"; ntdll="ntdll"; advapi32="advapi32"; ws2_32="ws2_32"; bcrypt="bcrypt"; userenv="userenv"; dbghelp="dbghelp"; secur32="secur32"; psapi="psapi"; shell32="shell32"; user32="user32"; ucrtbase="ucrt"; ole32="ole32"; oleaut32="oleaut32"; crypt32="crypt32"; ncrypt="ncrypt"; iphlpapi="iphlpapi"; kernelbase="kernelbase" }
foreach ($d in $map.Keys) {
  $path = "$env:WINDIR\System32\$d.dll"
  $names = @()
  foreach ($line in (& $dumpbin /nologo /exports $path)) {
    if ($line -match '^\s+\d+\s+[0-9A-F]+\s+[0-9A-F]{8}\s+(\S+)') { $names += $matches[1] }
    elseif ($line -match '^\s+\d+\s+[0-9A-F]+\s+(\S+)\s+\(forwarded') { $names += $matches[1] }
  }
  Set-Content -Path "$defs/$($map[$d]).def" -Value ("LIBRARY $d.dll`r`nEXPORTS`r`n" + (($names | ForEach-Object { "  $_" }) -join "`r`n"))
}
Set-Content "$defs/synchronization.def" "LIBRARY api-ms-win-core-synch-l1-2-0.dll`r`nEXPORTS`r`n  WaitOnAddress`r`n  WakeByAddressSingle`r`n  WakeByAddressAll"
foreach ($f in Get-ChildItem "$defs/*.def") {
  & $lld -flavor link /lib /nologo /machine:x64 "/def:$($f.FullName)" "/out:$kit/$($f.BaseName).lib" 2>&1 | Select-Object -First 3
}
$sys = "kernel32.lib","ntdll.lib","advapi32.lib","ws2_32.lib","bcrypt.lib","userenv.lib","dbghelp.lib","secur32.lib","psapi.lib","shell32.lib","user32.lib","synchronization.lib","ucrt.lib"

function Link($name, $objs, [string[]]$extra) {
  $log = "$Out/$name.log"
  & $lld -flavor link /nologo /NODEFAULTLIB /SUBSYSTEM:CONSOLE /INCREMENTAL:NO /DEBUG "/LIBPATH:$kit" "/OUT:$Out/$name.exe" "$kit/velt_crt.obj" $objs $sys $extra "/errorlimit:0" *> $log
  $code = $LASTEXITCODE
  Write-Host "link $name exit $code"
  Get-Content $log | Select-String "undefined symbol" | ForEach-Object { ($_.Line -replace '.*undefined symbol: ','').Trim() } | Sort-Object -Unique | Tee-Object "$Out/$name-undef.txt"
  Get-Content $log | Select-String -NotMatch "undefined symbol|>>> referenced|^\s*$" | Select-Object -First 15 | ForEach-Object { $_.Line }
}

Section "whole archive superset"
Link "whole" @() @("/WHOLEARCHIVE:$rt", "/FORCE:UNRESOLVED")
Get-Content "$Out/whole.log" | Select-String "undefined symbol" | ForEach-Object { ($_.Line -replace '.*undefined symbol: ','').Trim() } | Sort-Object -Unique | Set-Content "$Out/whole-undef.txt"
Write-Host "whole-archive undefined: $((Get-Content "$Out/whole-undef.txt").Count)"
Get-Content "$Out/whole-undef.txt"

Section "programs"
$progs = [ordered]@{ hello="tests/golden/m1/hello.vlt"; http="examples/http_hello.vlt"; all_std="bench/compile/all_std.vlt"; throws="tests/golden/bugs/lower_ctor_throws_after_sharing_this.vlt"; finally="tests/golden/lang/finally_after_return_keeps_values.vlt" }
foreach ($p in $progs.Keys) {
  & $velt build --emit obj $progs[$p] -o "$Out/$p.obj" 2>&1 | Select-Object -Last 3
  Link $p @("$Out/$p.obj", $rt) @()
}
foreach ($p in "hello","throws","finally","all_std") {
  Section "run $p"
  if (Test-Path "$Out/$p.exe") { & "$Out/$p.exe" 2>&1 | Select-Object -First 15; Write-Host "exit $LASTEXITCODE" }
  $expected = [IO.Path]::ChangeExtension($progs[$p], ".out")
  if (Test-Path $expected) { Write-Host "--- expected:"; Get-Content $expected | Select-Object -First 15 }
}
Section "run http"
$env:VELT_HELLO_SECONDS = "3"
$job = Start-Process -FilePath "$Out/http.exe" -PassThru -NoNewWindow -RedirectStandardOutput "$Out/http.out"
Start-Sleep 1
try { (Invoke-WebRequest -UseBasicParsing http://127.0.0.1:8080/json).Content } catch { Write-Host "request failed: $_" }
$job.WaitForExit(); Get-Content "$Out/http.out"; Write-Host "exit $($job.ExitCode)"
Remove-Item Env:VELT_HELLO_SECONDS

Section "shared runtime DLL linked with lld-link + shim (cargo rustc)"
$env:CARGO_TARGET_DIR = "target/lldlink"
# rust-lld picks the COFF flavor from its file name.
Copy-Item $lld "$Out/lld-link.exe"
cargo rustc --release -p velt_rt_shared -- -C "linker=$Out/lld-link.exe" -C "link-arg=/NODEFAULTLIB" -C "link-arg=/LIBPATH:$kit" -C "link-arg=$kit/velt_crt_dll.obj" -C "link-arg=ucrt.lib" -C "link-arg=synchronization.lib" -C "link-arg=psapi.lib" -C "link-arg=shell32.lib" -C "link-arg=user32.lib" 2>&1 | Select-Object -Last 25
Remove-Item Env:CARGO_TARGET_DIR
$dll = "target/lldlink/release/velt_rt_shared.dll"
if (Test-Path $dll) {
  & $dumpbin /nologo /dependents $dll | Select-String "\.dll" | ForEach-Object { $_.Line.Trim() }
  Copy-Item $dll "$Out/velt_rt_shared.dll"; Copy-Item "$dll.lib" "$Out/velt_rt_shared.dll.lib"
  & $velt build --emit obj tests/golden/m1/hello.vlt -o "$Out/dhello.obj"
}
foreach ($f in Get-ChildItem "$Out/*.exe") { Write-Host "$($f.Name): $(& $dumpbin /nologo /dependents $f.FullName | Select-String '\.dll' | ForEach-Object { $_.Line.Trim() })" }

Section "clean container (servercore, no VS / no vcruntime)"
docker version --format '{{.Server.Os}}' 2>&1
$img = "mcr.microsoft.com/windows/servercore:ltsc2025"
docker pull -q $img 2>&1 | Select-Object -Last 1
docker run --rm -v "${Out}:C:\o" $img cmd /c "dir C:\Windows\System32\vcruntime140.dll & C:\o\hello.exe & echo exit %errorlevel% & C:\o\all_std.exe & echo exit %errorlevel% & C:\o\throws.exe & echo exit %errorlevel%" 2>&1 | Select-Object -First 60
