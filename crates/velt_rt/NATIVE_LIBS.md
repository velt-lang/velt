# Linking a Velt executable against velt_rt

An executable is `<program object(s)> + velt_rt staticlib + native system libs`. The runtime defines
the C `main`; the program object defines `int32_t velt_main(void)` and nothing else is required.
The staticlib is `target/<profile>/velt_rt.lib` (MSVC) or `target/<profile>/libvelt_rt.a` (Unix).

Native libs below come from
`cargo rustc -p velt_rt --crate-type staticlib -- --print native-static-libs`.
Re-run it after changing dependencies. Re-checked after adding the async runtime (tokio, hyper 1.x,
hyper-util, parking_lot, socket2): the Windows list is unchanged (tokio/mio already need `ws2_32`
and `ntdll`, which were on it).

## Windows MSVC (x86_64-pc-windows-msvc) — verified on this host (VS 2022 Build Tools 14.44, rustc 1.93.1)

```
link.exe /NOLOGO /SUBSYSTEM:CONSOLE /OUT:prog.exe prog.obj velt_rt.lib ^
    psapi.lib shell32.lib user32.lib advapi32.lib bcrypt.lib kernel32.lib ntdll.lib ^
    userenv.lib ws2_32.lib dbghelp.lib secur32.lib msvcrt.lib
```

`secur32.lib` came with tokio-postgres (its `whoami` dependency, std/postgres).

- `/SUBSYSTEM:CONSOLE` is **required**: `main` lives in a library member, so link.exe cannot infer
  the subsystem/entry from the objects and fails with `LNK1561: entry point must be defined`
  (observed with the debug staticlib). With it, the CRT's `mainCRTStartup` (from `msvcrt.lib`, the
  dynamic UCRT, which rustc's `/defaultlib:msvcrt` selects) calls our `main`. `/ENTRY:mainCRTStartup`
  is optional (implied by the subsystem).
- `msvcrt.lib` (not `libcmt.lib`): Rust std is built for the dynamic CRT. Program objects should
  not carry their own default-lib directives (Cranelift objects don't; C objects need `/Zl` or `/MD`).
- link.exe must run with the MSVC environment (`LIB` pointing at the MSVC + Windows SDK lib dirs),
  e.g. via `cc::windows_registry::find(target, "link.exe")` (sets the env) or a vcvars shell.
  Beware of Git-for-Windows' `/usr/bin/link.exe` shadowing it on `PATH`.
- Add `/DEBUG` for PDBs in debug builds if wanted.
- aarch64-pc-windows-msvc: same list expected (not verified here).

## Linux (x86_64/aarch64-unknown-linux-gnu) — x86_64 verified (WSL2 Ubuntu 20.04, glibc 2.31, gcc 9.4, rustc 1.98)

```
cc -pie -o prog prog.o libvelt_rt.a -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc
```
`--print native-static-libs` prints exactly this list on that host (with tokio/hyper/mimalloc).
Newer glibc/rustc may print a shorter list such as `-lgcc_s -lc`; extra libs are harmless.
Link with `cc`/`gcc`/`clang` as the driver so crt1.o/crti.o provide `_start` → `main`.
`link_check.rs` passes with this command line. aarch64: same list expected (not verified).

## macOS (x86_64/aarch64-apple-darwin) — expected, not verified on this host

```
cc -o prog prog.o libvelt_rt.a -framework SystemConfiguration -framework CoreFoundation \
    -lSystem -lc -lm -liconv
```
The two frameworks came with tokio-postgres (its `whoami` dependency, std/postgres); aarch64
verified with `--print native-static-libs` (rustc 1.9x, macOS 26).

## Shared runtime (debug builds)
`velt` links debug builds against `crates/velt_rt_shared` instead: the same sources as a shared
library without `main` (`libvelt_rt_shared.so` / `.dylib`, `velt_rt_shared.dll` + import library
`velt_rt_shared.dll.lib`). The executable's `main` comes from a small generated object
(`velt_codegen_cl::emit_entry_object`) that calls `velt_rt_start(argc, argv, velt_main)`:
```
cc -pie -o prog prog.o prog.entry.o -L<dir> -lvelt_rt_shared -Wl,-rpath,<dir> <native libs>
link.exe ... prog.obj prog.entry.obj velt_rt_shared.dll.lib <native libs>   (DLL copied next to prog.exe)
```
x86_64 Linux verified (Ubuntu 24.04, all goldens in debug mode); macOS and Windows not yet.

## Verification
`crates/velt_rt/tests/link_check.rs` (runs under `cargo test -p velt_rt`) compiles tiny C objects
that only define `velt_main`, links them with exactly the command lines above, runs the executables
and checks stdout/stderr/exit codes (normal return, `velt_rt_panic` → 101, `velt_rt_exit`). It also
links an `async main` (C state machines using `velt_rt_block_on`, `velt_rt_sleep`, `velt_rt_spawn`),
proving the tokio runtime links with the same list, and checks that output reaches a pipe while
the program idles in an `await`.

## Allocator
The staticlib embeds mimalloc as Rust's global allocator (feature `mimalloc`, on by default; it
compiles its C sources with the MSVC/cc toolchain at build time and needs no extra native libs).
Build with `--no-default-features` to use the system allocator instead.
