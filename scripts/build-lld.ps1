# Windows counterpart of scripts/build-lld.sh: builds <OutDir>\lld.exe with the static CRT (/MT),
# so it needs no Visual C++ redistributable. Run from a shell where cmake and MSVC are available
# (CMake picks the newest Visual Studio generator, which finds MSVC itself).
param([Parameter(Mandatory)][string]$OutDir, [string]$Version = $(if ($env:LLVM_VERSION) { $env:LLVM_VERSION } else { "23.1.3" }))
$ErrorActionPreference = "Stop"
$work = if ($env:LLD_WORK) { $env:LLD_WORK } else { Join-Path $env:TEMP "velt-lld-$Version" }
New-Item -ItemType Directory -Force $OutDir, $work | Out-Null
$src = Join-Path $work "llvm-project-$Version.src"
if (-not (Test-Path $src)) {
  $archive = Join-Path $work "src.tar.xz"
  curl.exe -fsSL --retry 5 --retry-all-errors -o $archive "https://github.com/llvm/llvm-project/releases/download/llvmorg-$Version/llvm-project-$Version.src.tar.xz"
  # bsdtar (tar.exe) cannot create the symlinks some test directories hold: extract only what the build needs.
  tar.exe -xf $archive -C $work "llvm-project-$Version.src/llvm" "llvm-project-$Version.src/lld" "llvm-project-$Version.src/cmake" "llvm-project-$Version.src/libunwind/include" "llvm-project-$Version.src/third-party" "llvm-project-$Version.src/libc"
  Remove-Item $archive
}
$build = Join-Path $work "build"
cmake -S "$src/llvm" -B $build -A x64 -Thost=x64 `
  -DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded `
  -DLLVM_ENABLE_PROJECTS=lld "-DLLVM_TARGETS_TO_BUILD=X86;AArch64;WebAssembly" `
  -DLLVM_ENABLE_ZLIB=OFF -DLLVM_ENABLE_ZSTD=OFF -DLLVM_ENABLE_LIBXML2=OFF -DLLVM_ENABLE_DIA_SDK=OFF `
  -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_BENCHMARKS=OFF -DLLVM_INCLUDE_EXAMPLES=OFF `
  -DLLVM_INCLUDE_DOCS=OFF -DLLVM_ENABLE_ASSERTIONS=OFF -DLLVM_BUILD_TOOLS=OFF | Out-Null
if ($LASTEXITCODE) { exit $LASTEXITCODE }
cmake --build $build --target lld --config Release --parallel
if ($LASTEXITCODE) { exit $LASTEXITCODE }
Copy-Item (Join-Path $build "Release/bin/lld.exe") (Join-Path $OutDir "lld.exe")
& (Join-Path $OutDir "lld.exe") -flavor link --version
Get-Item (Join-Path $OutDir "lld.exe") | Select-Object FullName, Length | Format-List
