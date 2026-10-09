# Linking

`velt build` compiles in-process (Cranelift, or LLVM through clang) and then links the program's
objects with the runtime (`velt_rt`) into an executable. Linking used to need the system's
linker and SDK: Visual Studio Build Tools on Windows, Xcode's command line tools on macOS,
`build-essential` on Linux. Released toolchains now bring their own (#803): **LLVM's `lld`**
and, per target, a **link kit** that stands in for the SDK. This page explains how the pieces
were chosen and how they are made. Users' view: [Platforms](../tooling/platforms.md#the-bundled-linker).

Code: `crates/velt_link` (`bundled.rs` chooses the linker, `lld.rs` builds lld's arguments,
`kit/` describes and builds kits, `src/bin/velt-kit.rs` is the tool that builds them and
refreshes their lists), `crates/velt_link/kit/` (the kits' sources).

## Choosing the linker

`velt_link::link` uses the bundled linker when the toolchain has both `lib/velt/lld[.exe]` and a
complete kit for the target (`lib/targets/<triple>/kit.stamp` of the current format), and the
system linker (`link.exe`, `cc`) otherwise. `$VELT_LINKER` overrides: `bundled` (an error when
it is missing), `system`, or a path to a linker program given the system linker's arguments
(as before). In a source checkout there is no `lib/velt/lld`; the Rust toolchain's `rust-lld`
is the same program and stands in for it, so a kit written into `target/lib/targets/<host>`
(`cargo run -p velt_link --bin velt-kit -- build --target <host> --out target/lib/targets/<host>`)
is enough to try the bundled linker. `velt doctor` reports the choice and, for the system
linker, why the bundled one was not used. The link stamp includes the choice, so switching
relinks.

The bundled lld also links WebAssembly (`-flavor wasm`), ahead of `rust-lld` and `wasm-ld`.

## lld

One executable links all formats; `velt` runs it with `-flavor link|gnu|darwin|wasm`.

- **Built from source** by `scripts/build-lld.sh` / `build-lld.ps1` (LLVM 23, the version
  rustc's `rust-lld` has): only the `lld` target, the X86, AArch64 and WebAssembly backends, no
  zlib/zstd/libxml2, the C++ runtime linked statically (Linux: `-static-libstdc++`; Windows:
  `/MT`). It depends on nothing but the C library: `libSystem` and `libc++` on macOS (part of the
  OS), glibc 2.31+ on Linux (built in Debian 11), system DLLs on Windows. About 55 MB.
- Not `rust-lld`: on Linux and macOS it is a thin executable loading rustc's `libLLVM` shared
  library (140 MB); on Windows it is a 120 MB static build of all of LLVM's targets.
- Not LLVM's release archives: they are 0.8–1.9 GB, ship lld linked against a shared `libLLVM`
  on some platforms, and have no x86_64 macOS build.
- `.github/workflows/lld.yml` builds it per release target (20–60 minutes) and caches it until
  the scripts change; `release.yml` packages the result.

## Windows (`x86_64-pc-windows-msvc`)

The kit holds import libraries and Velt's startup object; executables import only system DLLs
and the Universal CRT (`ucrtbase.dll`, part of Windows 10 and later).

**Import libraries.** `crates/velt_link/kit/windows/*.def` list every export of the DLLs the
runtime imports (kernel32, ntdll, advapi32, ws2_32, bcrypt, userenv, dbghelp, secur32, psapi,
shell32, user32, the `api-ms-win-core-synch` API set for `WaitOnAddress`), of a few more that
native packages commonly use (ole32, oleaut32, crypt32, ncrypt, iphlpapi), and of
`ucrtbase.dll` (`ucrt.lib`). `velt-kit lists windows` reads them from `System32` (exports outside
executable sections are marked `DATA`); `velt-kit build` turns them into `.lib` files with
`lld -flavor link /lib /def:`. Lists of export names are what mingw-w64 ships too; no Microsoft
library file is redistributed. Programs link with `/NODEFAULTLIB`: the `/DEFAULTLIB` directives
the runtime's objects carry (`msvcrt`, `msvcprt`, `oldnames`, `uuid`) name Visual Studio's
libraries.

**The CRT question** (#803's open question, answered by linking hello, the HTTP server and the
whole runtime with lld-link and only generated import libraries on a GitHub runner, then
running the results in a Windows Server Core container without Visual Studio or
`vcruntime140.dll`). The runtime stays a `*-windows-msvc` build with the dynamic CRT (`/MD`).
With `ucrtbase.dll` imported, eleven symbols were left; Rust's MSVC panics need nothing more,
since `ucrtbase.dll` exports `__CxxFrameHandler3` and `_CxxThrowException`. The eleven are what
Visual Studio's CRT startup objects provide, and `kit/crt/windows_x86_64.rs` provides them
(`rustc --emit obj` of a `no_std` file; `velt_crt.obj` for executables, `velt_crt_dll.obj` for
DLLs):

| Symbols | Needed by | What the startup object does |
|---|---|---|
| `mainCRTStartup` | the executable's entry | sets up the UCRT's `argv` and environment (`_configure_narrow_argv`, `_initialize_narrow_environment`), runs the C and C++ initializers (`.CRT$XI*`, `.CRT$XC*`), calls `main`, `exit` |
| `_DllMainCRTStartup` | `velt_rt_shared.dll` | the same initializers, and the DLL's own `atexit` table |
| `_tls_used`, `_tls_index` | every `thread_local` (Rust's and C++'s) | the TLS directory: template bounds (`.tls`, `.tls$ZZZ`), index slot, callbacks (`.CRT$XL*`) |
| `__dyn_tls_on_demand_init`, `__tls_guard` | mimalloc, which MSVC compiles as C++ | runs C++ `thread_local` initializers (`.CRT$XD*`) once per thread |
| `__security_cookie`, `__security_check_cookie`, `__GSHandlerCheck`, `__report_rangecheckfailure` | SQLite and mimalloc (`/GS`) | a random cookie at start, `__fastfail` on mismatch |
| `` type_info::`vftable' `` | Rust's MSVC panic type descriptor | a vtable never called (exception matching compares names) |
| `std::get_new_handler` | mimalloc's `operator new` | no handler |
| `atexit` | the runtime's C code | `_crt_atexit` (executables), the module's table (DLLs) |

`/MD` vs `/MT` (`crt-static`) was not needed: `crt-static` would have needed Visual Studio's
static CRT libraries at every link. The `*-windows-gnullvm` target (mingw ABI) would have
changed the runtime's triple, debug info format and native package ABI.

**velt.exe and the shared runtime DLL** are built with Visual Studio (`/MD`) like any Rust
program, which makes them import `vcruntime140.dll`. `scripts/package.ps1` links both again with
the bundled lld-link and the kit (`cargo rustc -- -C linker=lld-link -C link-arg=/NODEFAULTLIB
…`), so the toolchain runs on a machine without the Visual C++ redistributable.

## Linux, glibc (`*-unknown-linux-gnu`)

Executables are dynamically linked against the system's glibc, as `cc` links them, and run on
glibc 2.31 or newer (the release toolchains' baseline, Debian 11).

- **Stub libraries.** `kit/linux/<arch>.txt` lists every exported symbol of `libc.so.6`,
  `libm.so.6`, `libpthread.so.0`, `libdl.so.2`, `librt.so.1`, `libutil.so.1`, `libgcc_s.so.1`
  and the dynamic loader, with its default version, type and (data) size, from Debian 11
  (`velt-kit lists linux`, `--root` reads an unpacked image of another architecture). `velt-kit
  build` writes an ELF object defining them (`object` crate) and links it into a shared library
  with lld (`-soname`, a version script): the executable records the same `NEEDED` entries and
  symbol versions a real link records.
- **`crt1.o`** (`kit/crt/linux_gnu.rs`): what `Scrt1.o`, `crti.o`/`crtn.o`, gcc's
  `crtbeginS.o` and `libc_nonshared.a` provide:
  - `_start`, which calls `__libc_start_main` with an `init` function running
    `.preinit_array` and `.init_array`. glibc before 2.34 does not run them itself, and Rust's
    `std` reads `argv` from an `.init_array` entry.
  - `__dso_handle` (hidden), for `__cxa_atexit` and thread-local destructors.
  - The `libc_nonshared.a` wrappers glibc 2.31 links statically: `stat64`/`fstat64`/
    `lstat64`/`fstatat64` and the non-64 names (→ `__xstat64` …), `mknod`/`mknodat`, `atexit`,
    `pthread_atfork`.
- Arguments: `-pie --dynamic-linker <loader> --eh-frame-hdr -z relro -z noexecstack
  --hash-style=gnu --build-id`, the stubs `--as-needed`. `--allow-shlib-undefined`, because the
  shared runtime was linked against the build machine's glibc and may bind newer versions.

## Linux, musl (`*-unknown-linux-musl`)

Fully static executables: `velt build --target x86_64-unknown-linux-musl` (or `aarch64-…`).
The kit is musl's `crt1.o`, `crti.o`, `crtbegin.o`, `crtend.o`, `crtn.o`, `libc.a` and
`libunwind.a` as the Rust toolchain ships them (`lib/rustlib/<triple>/lib/self-contained`), plus
the runtime built for musl (`libvelt_rt.a`, where `velt` looks for a non-host target's runtime).
The program's objects are the same as for glibc. No shared runtime: debug builds link the static
one too.

## macOS (`*-apple-darwin`)

`ld64.lld` with the kit as the SDK (`-syslibroot`): `usr/lib/libSystem.tbd` and the
`CoreFoundation` and `SystemConfiguration` frameworks' `.tbd`, TAPI text stubs listing the
symbols the runtime imports from them (`kit/macos.txt`: about 270 names, merged from arm64 and
x86_64 by `velt-kit lists macos`, which links the whole runtime against the SDK and reads the
binds). The stubs are written by Velt from those names; no file of Apple's SDK is copied. lld
signs arm64 executables ad hoc, as Apple's `ld` does. `-platform_version macos <min> <min>`
with the same minimums as before (11.0 arm64, 10.12 x86_64, or `MACOSX_DEPLOYMENT_TARGET`).

A native package's library that calls a `libSystem` function the runtime does not use fails to
link with the bundled linker; `VELT_LINKER=system` links it with Xcode's tools.

## Refreshing the lists

| List | Command | Where |
|---|---|---|
| `kit/windows/*.def` | `cargo run -p velt_link --bin velt-kit -- lists windows` | Windows |
| `kit/linux/<arch>.txt` | `velt-kit lists linux [--root <unpacked debian:bullseye>] [--arch …]` | Debian 11 |
| `kit/macos.txt` | `velt-kit lists macos --runtime target/release/libvelt_rt.a` | macOS, on arm64 and x86_64 |

Refresh `macos.txt` when the runtime starts using a new system function (links of programs then
fail with an undefined `_name`); the others list every export and change only with the baseline.

## Testing

- `crates/velt_link`: unit tests of the choice, the kits and every argument list;
  `tests/link_host.rs` links and runs a program with the system linker, with a kit it builds and
  the Rust toolchain's `rust-lld`, and through a `$VELT_LINKER` program.
- `release.yml` `clean-machine`: the packaged toolchain in `debian:bookworm-slim` (no
  `build-essential`; glibc and musl targets), in a Windows Server Core container (no Visual
  Studio, SDK or Visual C++ runtime) and on macOS, with `VELT_LINKER=bundled`, builds and runs
  hello and `examples/http_hello.vlt` in debug and release mode (`scripts/smoke-clean.*`).

## Cross-linking

lld links every format on every host, and the kits are plain files built from the checked-in
lists, so nothing in them depends on the build machine. `velt_link` already links for another OS
when the toolchain has a kit and a runtime for that target; what is missing is per-target
runtime packages to install, and a check that code generation makes no host assumptions.
