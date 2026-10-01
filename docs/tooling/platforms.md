# Platforms and installation

## Supported platforms

| Platform | Status |
|---|---|
| Windows x86_64 (MSVC) | Primary development platform; every test runs here. |
| Linux x86_64 (glibc) | Verified: the full test suite (all end-to-end tests in debug and release, release through LLVM), `velt doctor`, packaging and installation. Tested on Ubuntu 20.04 (WSL2) with clang 16–18, and in Debian 12 containers. |
| Linux aarch64 (glibc) | Verified: the full test suite in an Ubuntu 24.04 arm64 VM and in Debian 12 containers, database drivers included (PostgreSQL 17, Redis 7, Redis over TLS). |
| macOS arm64 (Apple silicon) | Verified: the full test suite (release through LLVM and, separately, through Cranelift), `velt doctor`, packaging and installation into a fresh prefix. Tested on macOS 26. |
| macOS x86_64 | Verified under Rosetta 2: every end-to-end test cross-built by the arm64 `velt` and run as x86_64. Not yet run on an Intel Mac. |
| Linux musl (Alpine) | Not an official target, but works: `velt doctor`, every end-to-end test and the HTTP example pass on Alpine 3.21 (aarch64). `velt dev`'s JIT host is unavailable there (use `velt dev --exe`). |
| WebAssembly | `wasm32-wasip1` and `wasm32-unknown-unknown`, single-threaded, without networking ([WebAssembly](webassembly.md)). |

Windows on arm64 has not been run yet. Its executables carry unwind information (`.pdata` and
`.xdata`, as on x64), so debuggers and backtraces walk through Velt frames; `velt dev`'s JIT
code does not register unwind information there yet.

## Prerequisites

- **Windows**: Visual Studio 2022 or the *Build Tools for Visual Studio* with the "Desktop
  development with C++" workload (the MSVC linker and the Windows SDK). `velt` finds `link.exe`
  through the registry and vswhere; no Developer Prompt is needed.
- **Linux**: a C toolchain (`sudo apt install build-essential` or `sudo dnf install gcc`).
- **macOS**: the Xcode command line tools (`xcode-select --install`).
- **Optional**: LLVM/clang **16 or newer** for optimized `--release` builds
  (`winget install LLVM.LLVM`, `brew install llvm`, `sudo apt install clang-18`). Without it,
  `--release` uses Cranelift. Older clang versions are skipped with a note, and `velt doctor`
  reports which clang it uses.

Building Velt itself needs Rust (stable); see [Getting started](../book/getting-started.md#install).

## The toolchain layout

A Velt toolchain is one self-contained directory (the **prefix**). Nothing is registered
globally: put `<prefix>/bin` on `PATH` and run `velt doctor`.

```
<prefix>/
  bin/velt[.exe]          the CLI (compiler, runner, package manager, test runner, fmt, lsp)
  lib/velt_rt.lib         runtime static library (Windows)
  lib/libvelt_rt.a        runtime static library (Linux, macOS)
  lib/velt_rt_shared.dll, lib/velt_rt_shared.dll.lib
                          runtime shared library + import library (Windows)
  lib/libvelt_rt_shared.so / lib/libvelt_rt_shared.dylib
                          runtime shared library (Linux / macOS)
  lib/NATIVE_LIBS.md      the system libraries programs link against
  std/**                  standard library sources
  README.md, LICENSE
```

How `velt` finds its parts (first match wins):

| Part | Search order |
|---|---|
| runtime library | `$VELT_RT_LIB` → next to the `velt` executable → its parent directory → `<exe dir>/../lib` |
| shared runtime (debug builds) | not used with `$VELT_RT_LIB` or `$VELT_RT_LINK=static` → next to the `velt` executable → its parent directory → `<exe dir>/../lib`; not found → debug builds link the static runtime |
| standard library | `$VELT_STD` → `<exe dir>/../std` when the executable is in a `bin/` directory → the `std/` of the source checkout it was built in → `<exe dir>/std` |
| linker | `$VELT_LINKER` → Windows: MSVC `link.exe` found through the registry and vswhere; Linux and macOS: `cc` (Linux static links use `-fuse-ld=mold` / `lld` when `mold` / `ld.lld` is on `PATH`) |
| clang (`--release`) | `$VELT_CLANG` → `clang` on `PATH` → standard install directories (`C:\Program Files\LLVM\bin`, Homebrew, `/usr/bin`, `clang-NN` on `PATH`); versions older than 16 are skipped |

**Debug builds link the shared runtime** (milliseconds instead of the seconds a static link of
the whole runtime takes): the executable loads it from the toolchain's `lib/` (an rpath on Linux
and macOS; on Windows `velt_rt_shared.dll` is copied next to the executable). Such an executable
needs the toolchain it was built with; ship `--release` builds, which link the runtime statically
and run anywhere. `VELT_RT_LINK=static` makes debug builds static too.

## Building and installing a distribution

From a source checkout:

```sh
pwsh scripts/package.ps1        # Windows → dist/velt-<version>-<host triple>/ + .zip
scripts/package.sh              # Linux, macOS → dist/velt-<version>-<host triple>/ + .tar.gz

pwsh scripts/install.ps1 -Dist dist\velt-0.1.0-x86_64-pc-windows-msvc [-Prefix <dir>]
scripts/install.sh dist/velt-0.1.0-x86_64-unknown-linux-gnu [<prefix>]
```

The package scripts run `cargo build --release` and assemble the layout above (options:
`-StdDir`/`--std-dir`, `-SkipBuild`/`--skip-build`, `-NoArchive`/`--no-archive`). The default
prefix is `%LOCALAPPDATA%\velt` on Windows and `~/.velt/toolchain` on Linux and macOS. The
installer replaces `bin/`, `lib/` and `std/` in the prefix and prints the command that adds
`<prefix>/bin` to `PATH`; it never edits `PATH` itself. An unpacked archive also works in place.

## Windows notes

- `velt dev` hands listening sockets to each version over a named pipe and runs programs in a
  job object, so they end with `velt dev`. JIT code registers its unwind information, so
  debuggers and backtraces walk through it.
- Microsoft Defender scans every new executable the first time it starts, which can take more
  than a second for a freshly linked program. This affects every `velt run` and every
  `velt dev --exe` version; the default JIT mode of `velt dev` is not affected.

## Linux notes

- Executables are position-independent and dynamically linked against glibc (`libc`, `libm`,
  `libpthread`, `libdl`, `libgcc_s`). They run on the build machine's glibc version or newer,
  so build distributable binaries on the oldest distribution you target.
- Release builds are stripped and linked with `--gc-sections`.
- The default `clang` of Ubuntu 20.04 and 22.04 (10 and 14) is too old. On 24.04,
  `sudo apt install clang` (18) works; on older releases install `clang-18` from
  [apt.llvm.org](https://apt.llvm.org). Versioned binaries (`clang-18`) are found
  automatically.

## macOS notes

- The Xcode command line tools provide `cc` (the linker driver) and Apple clang. Apple clang 15
  (Xcode 15) or newer is LLVM 16-based and works for `--release`; with an older Xcode,
  `brew install llvm` or set `VELT_CLANG`.
- Programs are built for macOS 11.0 on arm64 (10.12 on x86_64), the same as rustc's defaults,
  whichever backend compiled them. `MACOSX_DEPLOYMENT_TARGET` raises the minimum (e.g. `13.0`);
  values below those defaults are ignored, since the runtime library needs them.
- The linker ad-hoc signs every executable on Apple silicon, which is all a locally built
  program needs. Files downloaded with a browser get the quarantine attribute, and Gatekeeper
  refuses ad-hoc signed binaries; clear it with `xattr -dr com.apple.quarantine <dir>`.
- macOS checks every new executable the first time it starts, which costs about 200–300 ms per
  `velt run`. Terminals listed under System Settings → Privacy & Security → **Developer Tools**
  skip the check (if your terminal is not listed, run
  `sudo spctl developer-mode enable-terminal` to make the entry appear, enable it, and restart
  the terminal). `velt doctor` reports the first-launch time. `velt dev` in its default JIT
  mode is not affected.
- Cross-building: an arm64 `velt` builds x86_64 programs with `--target x86_64-apple-darwin`
  (with the x86_64 runtime library: `cargo build -p velt_rt --target x86_64-apple-darwin`, and
  `VELT_RT_LIB` pointing at it); they run under Rosetta 2.
