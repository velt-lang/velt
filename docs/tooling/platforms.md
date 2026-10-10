# Platforms and installation

## Supported platforms

| Platform | Status |
|---|---|
| Windows x86_64 (MSVC) | Primary development platform; every test runs here. |
| Linux x86_64 (glibc) | Verified: the full test suite (all end-to-end tests in debug and release, release through LLVM), `velt doctor`, packaging and installation. Tested on Ubuntu 20.04 (WSL2) with clang 16–18, and in Debian 12 containers. |
| Linux aarch64 (glibc) | Verified: the full test suite in an Ubuntu 24.04 arm64 VM and in Debian 12 containers, database drivers included (PostgreSQL 17, Redis 7, Redis over TLS). |
| macOS arm64 (Apple silicon) | Verified: the full test suite (release through LLVM and, separately, through Cranelift), `velt doctor`, packaging and installation into a fresh prefix. Tested on macOS 26. |
| macOS x86_64 | Verified under Rosetta 2: every end-to-end test cross-built by the arm64 `velt` and run as x86_64. Not yet run on an Intel Mac. |
| Linux musl (Alpine) | As a host, not an official target, but works: `velt doctor`, every end-to-end test and the HTTP example pass on Alpine 3.21 (aarch64). `velt dev`'s JIT host is unavailable there (use `velt dev --exe`). As a build target, every toolchain builds fully static executables with `--target <arch>-unknown-linux-musl` after `velt target add` ([Cross-compiling](#cross-compiling)). |
| WebAssembly | `wasm32-wasip1` and `wasm32-unknown-unknown`, single-threaded, without networking ([WebAssembly](webassembly.md)). |

Windows on arm64 has not been run yet. Its executables carry unwind information (`.pdata` and
`.xdata`, as on x64), so debuggers and backtraces walk through Velt frames; `velt dev`'s JIT
code does not register unwind information there yet.

## Prerequisites

**None** for a released toolchain: it brings its own linker (LLVM's `lld`) and link kits that
stand in for the system SDKs, so `velt build` and `velt build --release` work right after
installing, with no Visual Studio Build Tools, Xcode or `cc` ([The bundled linker](#the-bundled-linker)).

- **Optional**: the system linker, which `velt` uses when the toolchain has no bundled linker
  (a toolchain built without it, or `VELT_LINKER=system`):
  - **Windows**: Visual Studio 2022 or the *Build Tools for Visual Studio* with the "Desktop
    development with C++" workload (the MSVC linker and the Windows SDK). `velt` finds
    `link.exe` through the registry and vswhere; no Developer Prompt is needed.
  - **Linux**: a C toolchain (`sudo apt install build-essential` or `sudo dnf install gcc`).
  - **macOS**: the Xcode command line tools (`xcode-select --install`).
- **Optional**: LLVM/clang **16 or newer** for optimized `--release` builds
  (`winget install LLVM.LLVM`, `brew install llvm`, `sudo apt install clang-18`). Without it,
  `--release` uses Cranelift. Older clang versions are skipped with a note, and `velt doctor`
  reports which clang it uses.

Building Velt itself needs Rust (stable) and the system C toolchain above; see
[Getting started](../book/getting-started.md#install).

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
  lib/velt/lld[.exe]      the bundled linker (LLVM lld: COFF, ELF, Mach-O and WebAssembly),
                          with its license (lib/velt/LICENSE.txt)
  lib/targets/<triple>/   link kit per target: import or stub libraries of the system and the
                          startup objects; for other targets (target packs, `velt target add`)
                          also their runtime library
  std/**                  standard library sources
  README.md, LICENSE-MIT, LICENSE-APACHE, NOTICE
```

How `velt` finds its parts (first match wins):

| Part | Search order |
|---|---|
| runtime library | `$VELT_RT_LIB` → next to the `velt` executable → its parent directory → `<exe dir>/../lib` |
| shared runtime (debug builds) | not used with `$VELT_RT_LIB` or `$VELT_RT_LINK=static` → next to the `velt` executable → its parent directory → `<exe dir>/../lib`; not found → debug builds link the static runtime |
| standard library | `$VELT_STD` → `<exe dir>/../std` when the executable is in a `bin/` directory → the `std/` of the source checkout it was built in → `<exe dir>/std` |
| linker | `$VELT_LINKER` (`bundled`, `system` or a linker program) → the bundled lld (`<exe dir>/../lib/velt/lld`, in a checkout the Rust toolchain's `rust-lld`) with the target's kit (`lib/targets/<triple>/`) → the system linker: Windows: MSVC `link.exe` found through the registry and vswhere; Linux and macOS: `cc` (Linux static links use `-fuse-ld=mold` / `lld` when `mold` / `ld.lld` is on `PATH`) |
| clang (`--release`) | `$VELT_CLANG` → `clang` on `PATH` → standard install directories (`C:\Program Files\LLVM\bin`, Homebrew, `/usr/bin`, `clang-NN` on `PATH`); versions older than 16 are skipped |

**Debug builds link the shared runtime** (milliseconds instead of the seconds a static link of
the whole runtime takes): the executable loads it from the toolchain's `lib/` (an rpath on Linux
and macOS; on Windows `velt_rt_shared.dll` is copied next to the executable). Such an executable
needs the toolchain it was built with; ship `--release` builds, which link the runtime statically
and run anywhere. `VELT_RT_LINK=static` makes debug builds static too.

## The bundled linker

Released toolchains link programs with their own `lld` and a **link kit** per target, so no
system linker, SDK or C compiler is involved:

| Target | What the kit holds | Executables need at run time |
|---|---|---|
| Windows x64 | import libraries for the system DLLs and the Universal CRT, generated from export lists, and Velt's startup object (what Visual Studio's CRT objects provide) | Windows 10 or newer (the Universal CRT ships with it); no Visual C++ redistributable |
| Linux (glibc) | stub `libc.so.6`, `libm.so.6`, … with glibc 2.31's symbols and versions, and Velt's `crt1.o` | glibc 2.31 or newer |
| Linux (musl) | musl's startup objects and `libc.a` (from the Rust toolchain) and the runtime built for musl | nothing: fully static |
| macOS | `.tbd` stubs for `libSystem` and the two frameworks the runtime uses | macOS 11 (arm64) / 10.12 (x86_64) or newer |

`velt doctor` says which linker a build uses, and why the bundled one is not used when it is
not. `VELT_LINKER=system` uses the system linker instead (for example to link a native package
whose library needs a system library the kit does not cover); `VELT_LINKER=bundled` makes a
missing bundled linker an error instead of a fallback. A toolchain built from source has no
bundled linker unless the package script added it (`scripts/package.*`, which builds lld with
`scripts/build-lld.*`); in a checkout,
`cargo run -p velt_link --bin velt-kit -- build --target <host> --out target/lib/targets/<host>`
writes a kit that the checkout's `velt` uses with the Rust toolchain's `rust-lld`. How the kits
are made: [Linking](../internals/linking.md).

## Cross-compiling

Any toolchain builds for every released target, whatever it runs on: a Mac builds Windows and
Linux executables, Windows builds Linux and macOS ones, one CI runner builds them all. A target
other than this machine's needs its **target pack** (its runtime library and link kit,
downloaded from the same release):

```sh
velt target add x86_64-pc-windows-msvc        # into <prefix>/lib/targets/<triple>/
velt build --target x86_64-pc-windows-msvc app.vlt
velt build --release --target x86_64-unknown-linux-musl app.vlt   # static Linux executable
velt target list                              # this machine, installed and available targets
velt target remove x86_64-pc-windows-msvc
```

| Target | Built executable runs on |
|---|---|
| `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` | Linux with glibc 2.31 or newer |
| `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` | any Linux (static) |
| `aarch64-apple-darwin`, `x86_64-apple-darwin` | macOS 11 / 10.12 or newer (ad-hoc signed) |
| `x86_64-pc-windows-msvc` | Windows 10 or newer |

- Builds for another target link the runtime statically, also in debug mode (the shared runtime
  is this machine's), and with the bundled linker.
- `velt run --target` runs only programs for this machine's OS (another architecture of it may
  run under Rosetta 2 or QEMU); build the others and run them on their system.
- `--release` uses the LLVM backend for any target when clang is installed (clang emits the
  object only; no SDK is involved), else Cranelift.
- Packs are verified against the hashes the installed toolchain carries
  (`lib/targets/PACKS.sha256`, every target's packs of that release), so a pack replaced on the
  way is refused. A toolchain without that list (built from source) checks downloads against the
  release's `SHA256SUMS` instead, after checking that file's signature (`SHA256SUMS.sig`)
  against the velt release key built into velt, so a mirror or a replaced release can't
  substitute a pack either. `$VELT_INSTALL_PUBLIC_KEY` (hex) sets another key, for the releases
  of another build.
- A pack holds a runtime built with one velt: `velt build` uses it only with that velt (version
  and commit) and otherwise asks for `velt target add` again.
- `velt target add --from <pack.tar.gz>` installs a pack downloaded before, verified the same
  way (or against a `SHA256SUMS` beside it); one that cannot be verified, such as a pack you
  built yourself with `scripts/package.*` (`dist/velt-<version>-target-<triple>.tar.gz`), needs
  `--unverified`. `$VELT_INSTALL_BASE_URL` downloads from another repository's releases, for
  `velt target add` as for the installers.
- Each pack carries the licenses of what it contains (`NOTICE`, `LICENSE-MIT`, `LICENSE-APACHE`;
  musl's packs also musl's `COPYRIGHT` and LLVM's license).
- A package with a native library needs a prebuilt library for the target, which
  `velt build --target` fetches. Debug builds link it as a shared library from this machine's
  package cache, so for another machine build with `--release` (Linux and macOS link the
  library's object statically; Windows executables get its DLL beside them).

## Installing a release

Every [GitHub release](https://github.com/velt-lang/velt/releases) carries a toolchain archive
per platform, `SHA256SUMS`, and two installers that download the archive for this machine,
check its checksum and install it:

```sh
curl -fsSL https://github.com/velt-lang/velt/releases/latest/download/get-velt.sh | sh
```

```powershell
irm https://github.com/velt-lang/velt/releases/latest/download/get-velt.ps1 | iex
```

| Archive | Platform |
|---|---|
| `velt-<version>-x86_64-unknown-linux-gnu.tar.gz` | Linux x86_64, glibc 2.31 or newer (Ubuntu 20.04+, Debian 11+, RHEL 9+) |
| `velt-<version>-aarch64-unknown-linux-gnu.tar.gz` | Linux arm64, glibc 2.31 or newer |
| `velt-<version>-aarch64-apple-darwin.tar.gz` | macOS 11 or newer on Apple silicon |
| `velt-<version>-x86_64-apple-darwin.tar.gz` | macOS 10.12 or newer on Intel |
| `velt-<version>-x86_64-pc-windows-msvc.zip` | Windows x64 (also used on Windows arm64, under emulation) |

Each release also has a **target pack** per target, `velt-<version>-target-<triple>.tar.gz`
(also for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`), which
`velt target add` installs ([Cross-compiling](#cross-compiling)). There is no prebuilt toolchain
that runs on musl (Alpine) yet; build it from source there, or build static musl executables
anywhere with the musl target packs.

| `get-velt.sh` | `get-velt.ps1` | Environment variable | Meaning |
|---|---|---|---|
| `--version <v>` | `-Version <v>` | `VELT_INSTALL_VERSION` | the release to install (`0.1.0`); default: the release the installer belongs to |
| `--prefix <dir>` | `-Prefix <dir>` | `VELT_INSTALL_PREFIX` | where to install; default `~/.velt/toolchain`, `%LOCALAPPDATA%\velt` |
| `--archive <file>` | `-Archive <file>` | | install an archive you downloaded instead of downloading one |
| `--no-modify-path` | `-NoModifyPath` | | leave `PATH` alone |
| | | `VELT_INSTALL_BASE_URL` | the repository to download from (default `https://github.com/velt-lang/velt`) |

Pass options through the pipe with `sh -s --`, for example
`curl -fsSL .../get-velt.sh | sh -s -- --version 0.1.0`; in PowerShell set the environment
variables before `irm ... | iex`. Re-running the installer upgrades (or downgrades) in place: it
replaces `bin/`, `lib/` and `std/` in the prefix. To uninstall, delete the prefix and the `PATH`
line.

`PATH`: `get-velt.sh` appends `export PATH="<prefix>/bin:$PATH"` to `~/.profile`, to
`~/.bashrc`, `~/.bash_profile` and `~/.zshrc` when they exist (or `~/.zshrc` when your shell is
zsh), and adds `~/.config/fish/conf.d/velt.fish` when fish is set up; `get-velt.ps1` adds
`<prefix>\bin` to the user `Path`. Open a new terminal afterwards.

## Building and installing a distribution

From a source checkout:

```sh
pwsh scripts/package.ps1        # Windows → dist/velt-<version>-<host triple>/ + .zip
scripts/package.sh              # Linux, macOS → dist/velt-<version>-<host triple>/ + .tar.gz

pwsh scripts/install.ps1 -Dist dist\velt-0.1.0-x86_64-pc-windows-msvc [-Prefix <dir>]
scripts/install.sh dist/velt-0.1.0-x86_64-unknown-linux-gnu [<prefix>]
```

The package scripts run `cargo build --release` and assemble the layout above (options:
`-StdDir`/`--std-dir`, `-SkipBuild`/`--skip-build`, `-NoArchive`/`--no-archive`). They bundle
the lld given with `-Lld`/`--lld`, or build one with `scripts/build-lld.*` (CMake and Ninja or
Visual Studio; 20–60 minutes, cached under `~/.cache/velt` or `%LOCALAPPDATA%\velt`), and the
link kits; `-NoBundledLinker`/`--no-bundled-linker` leaves both out. On Windows they also link
`velt.exe` and `velt_rt_shared.dll` with the bundled linker, so the toolchain itself needs no
Visual C++ redistributable. The default
prefix is `%LOCALAPPDATA%\velt` on Windows and `~/.velt/toolchain` on Linux and macOS. The
installer replaces `bin/`, `lib/` and `std/` in the prefix and prints the command that adds
`<prefix>/bin` to `PATH`; it never edits `PATH` itself. An unpacked archive also works in place.

## Windows notes

- Executables import only system DLLs and the Universal CRT (`ucrtbase.dll`, part of Windows 10
  and later); with the bundled linker they need no Visual C++ redistributable
  (`vcruntime140.dll`).
- `velt dev` hands listening sockets to each version over a named pipe and runs programs in a
  job object, so they end with `velt dev`. JIT code registers its unwind information, so
  debuggers and backtraces walk through it.
- Microsoft Defender scans every new executable the first time it starts, which can take more
  than a second for a freshly linked program. This affects every `velt run` and every
  `velt dev --exe` version; the default JIT mode of `velt dev` is not affected.

## Linux notes

- Executables are position-independent and dynamically linked against glibc (`libc`, `libm`,
  `libpthread`, `libdl`, `libgcc_s`). Linked with the bundled linker they run on glibc 2.31 or
  newer whatever the build machine has; with the system linker, on the build machine's glibc
  version or newer.
- `--target x86_64-unknown-linux-musl` (or `aarch64-…` on arm64) builds a fully static
  executable with musl instead: it runs on any Linux of that architecture (Alpine, distroless
  and `scratch` containers included). Debug builds of musl programs link the static runtime too.
- Release builds are stripped and linked with `--gc-sections`.
- The default `clang` of Ubuntu 20.04 and 22.04 (10 and 14) is too old. On 24.04,
  `sudo apt install clang` (18) works; on older releases install `clang-18` from
  [apt.llvm.org](https://apt.llvm.org). Versioned binaries (`clang-18`) are found
  automatically.

## macOS notes

- With the bundled linker, building needs neither Xcode nor its command line tools. Those
  provide `cc` (the system linker driver) and Apple clang. Apple clang 15
  (Xcode 15) or newer is LLVM 16-based and works for `--release`; with an older Xcode,
  `brew install llvm` or set `VELT_CLANG`.
- Programs are built for macOS 11.0 on arm64 (10.12 on x86_64), the same as rustc's defaults,
  whichever backend compiled them. `MACOSX_DEPLOYMENT_TARGET` raises the minimum (e.g. `13.0`);
  values below those defaults are ignored, since the runtime library needs them.
- The linker (bundled or Xcode's) ad-hoc signs every executable on Apple silicon, which is all
  a locally built program needs. Files downloaded with a browser get the quarantine attribute,
  and Gatekeeper refuses ad-hoc signed binaries; clear it with
  `xattr -dr com.apple.quarantine <dir>`.
- macOS checks every new executable the first time it starts, which costs about 200–300 ms per
  `velt run`. Terminals listed under System Settings → Privacy & Security → **Developer Tools**
  skip the check (if your terminal is not listed, run
  `sudo spctl developer-mode enable-terminal` to make the entry appear, enable it, and restart
  the terminal). `velt doctor` reports the first-launch time. `velt dev` in its default JIT
  mode is not affected.
- Cross-building: after `velt target add x86_64-apple-darwin`, an arm64 `velt` builds x86_64
  programs with `--target x86_64-apple-darwin`; they run under Rosetta 2, so `velt run --target`
  works too ([Cross-compiling](#cross-compiling)).
