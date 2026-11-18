# Linking a Velt executable against velt_rt

An executable is `<program object(s)> + velt_rt staticlib + native system libs`. The runtime defines
the C `main`; the program object defines `int32_t velt_main(void)` and nothing else is required.
The staticlib is `velt_rt.lib` (MSVC) or `libvelt_rt.a` (Unix); a cargo build puts it in
`target/<profile>/`, a release toolchain in `lib/`.

`velt` links with the toolchain's bundled `lld` and its link kit for the target when the toolchain
has both, so no system linker, SDK or C compiler is needed (`crates/velt_link/src/lld.rs`,
`crates/velt_link/src/kit/`). Otherwise it uses the system linker with the command lines below
(`crates/velt_link/src/system.rs` holds the lists). They are rustc's
`cargo rustc -p velt_rt --crate-type staticlib -- --print native-static-libs` for the runtime;
re-run it after changing the runtime's dependencies.

## Windows MSVC (x86_64-pc-windows-msvc)

```
link.exe /NOLOGO /SUBSYSTEM:CONSOLE /OUT:prog.exe prog.obj velt_rt.lib ^
    kernel32.lib advapi32.lib ntdll.lib userenv.lib ws2_32.lib bcrypt.lib dbghelp.lib ^
    synchronization.lib msvcrt.lib
```

- `/SUBSYSTEM:CONSOLE` is **required**: `main` lives in a library member, so link.exe cannot infer
  the subsystem/entry from the objects and fails with `LNK1561: entry point must be defined`.
  With it, the CRT's `mainCRTStartup` (from `msvcrt.lib`, the dynamic UCRT, which rustc's
  `/defaultlib:msvcrt` selects) calls the runtime's `main`. `/ENTRY:mainCRTStartup` is optional
  (implied by the subsystem).
- `msvcrt.lib` (not `libcmt.lib`): Rust std is built for the dynamic CRT. Program objects should
  not carry their own default-lib directives (Cranelift objects don't; C objects need `/Zl` or `/MD`).
- link.exe must run with the MSVC environment (`LIB` pointing at the MSVC + Windows SDK lib dirs),
  e.g. via `cc::windows_registry::find(target, "link.exe")` (sets the env) or a vcvars shell.
  Beware of Git-for-Windows' `/usr/bin/link.exe` shadowing it on `PATH`.
- `velt` adds `/INCREMENTAL:NO` and a stack size, `/DEBUG` for debug builds (a PDB), and
  `/OPT:REF /OPT:ICF` for release builds.

## Linux (x86_64/aarch64-unknown-linux-gnu)

```
cc -pie -o prog prog.o libvelt_rt.a -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc
```

Newer glibc/rustc may print a shorter list such as `-lgcc_s -lc`; extra libs are harmless.
Link with `cc`/`gcc`/`clang` as the driver so crt1.o/crti.o provide `_start` → `main`. `velt`
adds `-Wl,--gc-sections -s` for release builds.

## macOS (x86_64/aarch64-apple-darwin)

```
cc -o prog prog.o libvelt_rt.a -liconv -lc -lm
```

`cc` always links libSystem, so `-lSystem` from rustc's list is left out (passing it again makes
ld64 warn about duplicate libraries). `velt` adds `-Wl,-dead_strip` for release builds. Frameworks
a package's native library needs come from its bundle (`system_libs` in its `native.json`,
[native_abi.md](../../docs/internals/contracts/native_abi.md) "Bundles"), so only programs that use
the package link them.

## Shared runtime (debug builds)

`velt` links debug builds against the shared runtime when it is installed
(`crates/velt_rt_shared`): the same sources as a shared library without `main`
(`libvelt_rt_shared.so` / `.dylib`, `velt_rt_shared.dll` + import library
`velt_rt_shared.dll.lib`). The executable's `main` comes from a small generated object
(`velt_codegen_cl::emit_entry_object`) that calls `velt_rt_start(argc, argv, velt_main)`:

```
cc -pie -o prog prog.o prog.entry.o -L<dir> -lvelt_rt_shared -Wl,-rpath,<dir> -lm -lc
link.exe ... prog.obj prog.entry.obj velt_rt_shared.dll.lib <native libs>   (DLL copied next to prog.exe)
```

The program's own objects call only the runtime and the C library, so the native library list is
shorter (`shared_native_libs` in `crates/velt_link/src/system.rs`). Release builds and
`$VELT_RT_LIB` use the static runtime.

## Verification

`crates/velt_rt/tests/link_check.rs` (runs under `cargo test -p velt_rt`) compiles tiny C objects
that only define `velt_main`, links them with the system linker and runs the executables, checking
stdout/stderr/exit codes (normal return, `velt_rt_panic` → 101, `velt_rt_exit`). It also links an
`async main` (C state machines using `velt_rt_block_on`, `velt_rt_sleep`, `velt_rt_spawn`),
proving the tokio runtime links, and checks that output reaches a pipe while the program idles in
an `await`.

## Allocator

The staticlib embeds mimalloc as Rust's global allocator (feature `mimalloc`, on by default; it
compiles its C sources with the MSVC/cc toolchain at build time and needs no extra native libs).
Build with `--no-default-features` to use the system allocator instead. Release builds for a
native target call mimalloc's `mi_*` functions directly when the runtime they link has them
([rt_abi.md](../../docs/internals/contracts/rt_abi.md) "Memory"); with a runtime without
mimalloc they call `velt_rt_alloc` / `velt_rt_free`.
