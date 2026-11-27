# Prebuilt C libraries of packages — CONTRACT

How the C libraries a package builds from `sources` ([c-ffi.md](../design/c-ffi.md) "Where
libraries come from") are published prebuilt, installed, verified and linked. Implemented by
`crates/vpm` (`vpm::native`: bundles, checks, `c_build`), the compiler (`crates/veltc/src/native/`)
and `crates/velt_link` (`NativeLink`).

`declare c` is the only way to call native code. The Velt-specific native ABI that packages' Rust
crates used (the runtime function table `velt_native_init_<pkg>` received, `velt_<pkg>__` exports
with signature records, handle tables, row cursors and parameter sinks, the `velt_native` SDK) was
removed in [#2563](https://github.com/velt-lang/velt/issues/2563); a manifest or a bundle of one
is refused with an error saying to use `declare c`.

## Bundles

A bundle is one target's C libraries of one package version, a directory exchanged as a
`VELTPKG1` archive (`vpm::archive`) restricted to these paths:

```text
native.json                        # package, version, target, abi (0), exports ({}), c
shared/libvelt_c_<p>__<lib>.so     # .dylib on macOS
static/libvelt_c_<p>__<lib>.a      # Linux and macOS
shared/velt_c_<p>__<lib>.dll       # Windows, with shared/velt_c_<p>__<lib>.dll.lib
```

- `velt native build [--target <triple>]` compiles the package's `sources` with the pinned zig
  (`vpm::native::c_build`) into `<package>/target/velt-native/<triple>/`; a bundle is reused
  while the hash of its inputs is unchanged (c-ffi.md).
- `native.json`'s `c` maps each library name the package's declarations give to its files,
  under exactly the names above (`vpm::native::c_library_files`): on Windows the DLL is copied
  beside the executable, so a bundle can't name another package's. Every file it names must be in
  the bundle. Checked on unpack, publish and load (`vpm::native::bundle::check_meta`).
- `abi` is 0 and `exports` empty: C libraries call no runtime function and have no init. Both
  fields stay so that an older velt reads the bundle. A bundle with no `c`, an `abi` above 0 or
  exports is a Rust crate's library: refused.
- The checksum of a bundle is the content hash of its files (`vpm::native::bundle::checksum`,
  the hash `velt.lock.json` records for packages).

## Install

For the build target (`InstallOptions::target`):
- A **registry** package with `native` checksums in the lockfile: the bundle for the target is
  fetched into `<cache>/native/<name>-<version>/<triple>/`, after its checksum (and its
  `native.json`'s package, version and target) is verified in a staging directory; nothing
  unverified is ever where the compiler loads or links it. A cached bundle whose files changed is
  replaced. The index's `native_abi` above 0 is refused before download.
- No bundle for the target: built from the package's sources with zig, when the program links
  them, only when the user opted in with `VELT_NATIVE_FROM_SOURCE=1`. Otherwise the error

  ```text
  `tiny 0.1.0` has no prebuilt native library for <triple> (published: <targets>).
  Set VELT_NATIVE_FROM_SOURCE=1 to build its C sources here with the pinned zig, or ask the package author to publish this target.
  ```
- A **path** package (or the root package) builds its sources into
  `<package>/target/velt-native/<triple>/`; a git or url package only with
  `VELT_NATIVE_FROM_SOURCE=1`.
- The result is `vpm::GraphPackage::c_bundle`: `CBundle::Prebuilt` (a verified bundle) or
  `CBundle::FromSource` (where it builds). `velt add`/`velt install` list every package with a
  prebuilt bundle.

## Linking (`velt_link::NativeLink`)

Only the libraries a program's declarations name are linked.

| Build | Linux / macOS | Windows |
|---|---|---|
| debug | `-L<dir> -lvelt_c_<p>__<lib> -Wl,-rpath,<dir>` | import library; the DLL is copied next to the executable |
| `--release` | `static/libvelt_c_<p>__<lib>.a` after the program's objects: self-contained | as debug (no partial link with MSVC) |
| `velt dev` (JIT) | the shared library loaded with `RTLD_GLOBAL` | `LoadLibraryW` |

The bundled linker (lld with a link kit, [linking](../linking.md)) gets the same libraries
(`-rpath <dir>` instead of `-Wl,-rpath,<dir>`); a static musl executable
(`--target <arch>-unknown-linux-musl`) always takes the static library. The link stamp hashes
every library file (path, size, modification time). `velt dev` watches the C sources of
libraries built here and restarts (never swaps) when they change.
