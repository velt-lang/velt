# Windows counterpart of scripts/build-lld.sh: builds <OutDir>\lld.exe with the static CRT (/MT),
# so it needs no Visual C++ redistributable. Run from a shell where cmake and MSVC are available
# (CMake picks the newest Visual Studio generator, which finds MSVC itself).
# LLVM_VERSION / -Version picks another release; it needs LLVM_SHA256 / -Sha256, the SHA-256 of
# its llvm-project-<version>.src.tar.xz, which the download is checked against.
param(
  [Parameter(Mandatory)][string]$OutDir,
  [string]$Version = $(if ($env:LLVM_VERSION) { $env:LLVM_VERSION } else { "23.1.3" }),
  [string]$Sha256 = $env:LLVM_SHA256
)
$ErrorActionPreference = "Stop"
# The SHA-256 GitHub lists for llvm-project-23.1.3.src.tar.xz (as in build-lld.sh).
if (-not $Sha256) {
  if ($Version -ne "23.1.3") { throw "LLVM version $Version needs -Sha256 (or LLVM_SHA256): the SHA-256 of its source archive" }
  $Sha256 = "c44186a7762ed28954be72e5ff6df9808e0779d4f1bf014ecc4e7e211d31ee34"
}
$work = if ($env:LLD_WORK) { $env:LLD_WORK } else { Join-Path $env:TEMP "velt-lld-$Version" }
New-Item -ItemType Directory -Force $OutDir, $work | Out-Null
$src = Join-Path $work "llvm-project-$Version.src"
if (-not (Test-Path $src)) {
  $archive = Join-Path $work "src.tar.xz"
  curl.exe -fsSL --retry 5 --retry-all-errors -o $archive "https://github.com/llvm/llvm-project/releases/download/llvmorg-$Version/llvm-project-$Version.src.tar.xz"
  $Actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
  if ($Actual -ne $Sha256.ToLowerInvariant()) {
    Remove-Item $archive
    throw "llvm-project-$Version.src.tar.xz has SHA-256 $Actual, expected $Sha256"
  }
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
