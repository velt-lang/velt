#!/usr/bin/env bash
# Build the lld that release archives bundle (#803): one self-contained executable that links
# COFF (lld-link), Mach-O (ld64.lld), ELF (ld.lld) and WebAssembly (wasm-ld), chosen by argv[0]
# or `-flavor`. Only the X86, AArch64 and WebAssembly backends; no zlib/zstd/libxml2, the C++
# runtime linked statically, so the result runs on a machine without a compiler toolchain.
#
#   scripts/build-lld.sh <out-dir>        # writes <out-dir>/lld (and prints its size)
#
# LLVM_VERSION picks the release (default below: the LLD major version rustc ships with), with
# LLVM_SHA256 the SHA-256 of its llvm-project-<version>.src.tar.xz: the source archive is checked
# against it before anything is built. Windows: scripts/build-lld.ps1.
set -euo pipefail
OUT=${1:?usage: build-lld.sh <out-dir>}
VERSION=${LLVM_VERSION:-23.1.3}
# The SHA-256 GitHub lists for llvm-project-23.1.3.src.tar.xz (llvm/llvm-project release assets).
DEFAULT_SHA256=c44186a7762ed28954be72e5ff6df9808e0779d4f1bf014ecc4e7e211d31ee34
if [ -n "${LLVM_VERSION:-}" ] && [ -z "${LLVM_SHA256:-}" ]; then
  echo "error: LLVM_VERSION=$LLVM_VERSION needs LLVM_SHA256 (the SHA-256 of its source archive)" >&2
  exit 1
fi
SHA256=${LLVM_SHA256:-$DEFAULT_SHA256}
WORK=${LLD_WORK:-${TMPDIR:-/tmp}/velt-lld-$VERSION}
mkdir -p "$OUT" "$WORK"
OUT=$(cd "$OUT" && pwd)
src="$WORK/llvm-project-$VERSION.src"
if [ ! -d "$src" ]; then
  curl -fsSL --retry 5 --retry-all-errors -o "$WORK/src.tar.xz" \
    "https://github.com/llvm/llvm-project/releases/download/llvmorg-$VERSION/llvm-project-$VERSION.src.tar.xz"
  if command -v sha256sum >/dev/null; then actual=$(sha256sum "$WORK/src.tar.xz" | cut -d' ' -f1)
  else actual=$(shasum -a 256 "$WORK/src.tar.xz" | cut -d' ' -f1); fi
  if [ "$actual" != "$SHA256" ]; then
    rm -f "$WORK/src.tar.xz"
    echo "error: llvm-project-$VERSION.src.tar.xz has SHA-256 $actual, expected $SHA256" >&2
    exit 1
  fi
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
