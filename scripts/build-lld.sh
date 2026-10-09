#!/usr/bin/env bash
# Build the lld that release archives bundle (#803): one self-contained executable that links
# COFF (lld-link), Mach-O (ld64.lld), ELF (ld.lld) and WebAssembly (wasm-ld), chosen by argv[0]
# or `-flavor`. Only the X86, AArch64 and WebAssembly backends; no zlib/zstd/libxml2, the C++
# runtime linked statically, so the result runs on a machine without a compiler toolchain.
#
#   scripts/build-lld.sh <out-dir>        # writes <out-dir>/lld (and prints its size)
#
# LLVM_VERSION picks the release (default below: the LLD major version rustc ships with).
# Windows: scripts/build-lld.ps1.
set -euo pipefail
OUT=${1:?usage: build-lld.sh <out-dir>}
VERSION=${LLVM_VERSION:-23.1.3}
WORK=${LLD_WORK:-${TMPDIR:-/tmp}/velt-lld-$VERSION}
mkdir -p "$OUT" "$WORK"
OUT=$(cd "$OUT" && pwd)
src="$WORK/llvm-project-$VERSION.src"
if [ ! -d "$src" ]; then
  curl -fsSL --retry 5 --retry-all-errors -o "$WORK/src.tar.xz" \
    "https://github.com/llvm/llvm-project/releases/download/llvmorg-$VERSION/llvm-project-$VERSION.src.tar.xz"
  tar -xJf "$WORK/src.tar.xz" -C "$WORK" \
    "llvm-project-$VERSION.src/llvm" "llvm-project-$VERSION.src/lld" \
    "llvm-project-$VERSION.src/cmake" "llvm-project-$VERSION.src/libunwind/include" \
    "llvm-project-$VERSION.src/third-party" "llvm-project-$VERSION.src/libc"
  rm "$WORK/src.tar.xz"
fi
extra=()
case "$(uname -s)" in
  Linux) extra+=(-DCMAKE_EXE_LINKER_FLAGS="-static-libstdc++ -static-libgcc") ;;
  Darwin)
    case "$(uname -m)" in arm64) extra+=(-DCMAKE_OSX_DEPLOYMENT_TARGET=11.0) ;; *) extra+=(-DCMAKE_OSX_DEPLOYMENT_TARGET=10.15) ;; esac ;;
esac
gen=(); command -v ninja >/dev/null && gen=(-G Ninja)
cmake -S "$src/llvm" -B "$WORK/build" "${gen[@]}" \
  -DCMAKE_BUILD_TYPE=Release \
  -DLLVM_ENABLE_PROJECTS=lld \
  -DLLVM_TARGETS_TO_BUILD="X86;AArch64;WebAssembly" \
  -DLLVM_ENABLE_ZLIB=OFF -DLLVM_ENABLE_ZSTD=OFF -DLLVM_ENABLE_LIBXML2=OFF \
  -DLLVM_ENABLE_TERMINFO=OFF -DLLVM_ENABLE_LIBEDIT=OFF -DLLVM_ENABLE_LIBPFM=OFF \
  -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_BENCHMARKS=OFF -DLLVM_INCLUDE_EXAMPLES=OFF \
  -DLLVM_INCLUDE_DOCS=OFF -DLLVM_ENABLE_ASSERTIONS=OFF -DLLVM_BUILD_TOOLS=OFF \
  "${extra[@]}" >/dev/null
cmake --build "$WORK/build" --target lld --parallel "$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu)"
cp "$WORK/build/bin/lld" "$OUT/lld"
strip "$OUT/lld" 2>/dev/null || true
"$OUT/lld" -flavor gnu --version
ls -la "$OUT/lld"
if [ "$(uname -s)" = Darwin ]; then otool -L "$OUT/lld"; else ldd "$OUT/lld" || true; fi
