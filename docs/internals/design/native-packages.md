# Native code in packages

Status: implemented. Issue: [#22](https://github.com/velt-lang/velt/issues/22).

## Summary

A package can ship a Rust crate next to its Velt sources. The crate is built by the package
author when they publish; users download a **prebuilt library for their target** from the
registry, in the style of npm's prebuilt addons and esbuild's per-platform packages. Users never
need cargo or a Rust toolchain.

The exact interface (pass modes, the function table, signature records, bundle files, start-up,
linking) is the contract [native_abi.md](../contracts/native_abi.md). This note explains the
design behind it. Packages that bind a C library rather than ship Rust use `declare c` and
C `sources` instead ([c-ffi.md](c-ffi.md)); most packages are plain Velt and use neither.

How a program uses the library depends on the mode:

| Mode | How the native library is used |
|---|---|
| `velt dev` (JIT host) | loaded into the running host (`dlopen` / `LoadLibraryW`), nothing linked |
| `velt build` / `velt run` (debug) | linked as a shared library |
| `velt build --release`, Linux and macOS | linked statically as one prelinked object |
| `velt build --release`, Windows | linked against the DLL, which is placed next to the executable |

## Design

### Package layout and manifest

```text
mypkg/
  package.velt
  src/lib.velt          # the Velt API; `declare function velt_mypkg__open(...)` etc.
  native/Cargo.toml     # [lib] name = "velt_native_mypkg"; depends on `velt_native`
  native/src/lib.rs
```

The manifest's `native` field (`crates/vpm/src/manifest/mod.rs`, `NativeConfig`) names the
crate directory and the targets the author publishes for:

```ts
native: {
  path: "native",                       // the crate's directory (default "native")
  targets: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"],
},
```

- `path` is one directory name in the package root, not `src` or `target`.
- Every target is one `velt` itself builds for (`NATIVE_TARGETS`).
- `wasm: true` is rejected: packages with native code cannot target WebAssembly.

### Calls from Velt into the library

Velt calls the library through ordinary `declare function` and `declare async function`
declarations, with the pass modes std's own runtime functions use: scalars by value, strings,
arrays and structs of values as borrowed pointers, non-scalar results through a trailing
out-pointer, errors as `IoResult<T>` / `IoStatus`, and native objects as `u64` handles that the
package wraps in a class with `[Symbol.dispose]()`. Nothing in the parser, HIR or VIR is specific
to native packages.

Classes, interfaces, unions, closures and functions cannot cross the boundary. That rule also
keeps hot reload sound (below): native code never holds a Velt code pointer.

### Export names

Every export of package `p` starts with `velt_p__` (with `-` in the package name written `_`),
plus one init function, `velt_native_init_p`. The prefix is structural: no C library function
starts with `velt_`, so an export never takes the name of one, and package names never contain a
doubled, mixed or trailing `-`/`_`, so one package's prefix is never the start of another's.
Names that differ only in `-` versus `_` count as the same package name.

A `declare` outside std must name an export of **its own** package's library, with exactly the
signature the library recorded. The SDK's `#[velt_native::export]` emits a signature record for
each export (`velt_sig_<name>`), `velt native build` reads them from the built library into the
bundle's `native.json`, and the driver checks every `declare` against them after type checking
(`crates/veltc/src/native/`). A mismatch is a compile error at the `declare`, not a link error or
a crash later. As a result, two packages cannot bind each other's native code and their symbols
never collide.

### How native code reaches the runtime: a function table

The library never links a `velt_rt_*` symbol by name. Its init function receives a pointer to a
`#[repr(C)]` table, `VeltRtApi`, whose entries are documented runtime functions (allocating and
reading strings and byte arrays, creating futures, completing them, a fatal error, registering
row cursors). A table is used because it is the only scheme that works the same in all modes:

- a library `dlopen`ed into the `velt dev` host cannot see the host's runtime symbols, which are
  linked statically and not exported;
- a Windows DLL cannot import from an executable without an import library for it;
- a release executable has no shared runtime to link against;
- the table carries a version that the library checks before its first call.

The table is append-only: `abi_version` grows by one with each addition, and fields are never
removed or reordered. A library records the version it needs (`abi` in `native.json`,
`native_abi` in the registry index). vpm refuses to install a library that needs a newer table
than the running `velt` provides, and the SDK's init checks `abi_version` and `size` again.

Each package has its own init function rather than one global `velt_native_init`, which would
collide in a static link. The generated `velt_main` calls every package's init before user code,
with `velt_rt_native_api()`, and checks the result with `velt_rt_native_check`, which reports a
failing init as a start-up error naming the package (`LowerOptions::native_inits` in
`crates/velt_vir`). The JIT host resolves the init like any other export, so all modes start the
same way.

### Memory and ownership

- Strings and byte arrays that cross into Velt are allocated through the table, by the runtime's
  allocator, so generated code adopts them like its own.
- Arguments are borrowed for the duration of the call; the SDK rejects parameter types that
  would outlive it (`&'static str`). Native code that keeps a value copies it.
- Native objects live behind `u64` handles that are keys in a registry with generations, never
  addresses: a forged or stale handle fails instead of reaching memory.
- A panic never unwinds across `extern "C"`: the SDK's export wrapper turns it into an
  `IoResult` error, or into a fatal error for functions that cannot fail.

### Async

The library has its own copy of Rust's standard library and cannot share the runtime's
executor. The table offers two ways to produce a future the runtime polls:

- `fut_blocking` runs a `work` function on the runtime's blocking pool and completes the future
  with its result. This fits synchronous C-library wrappers.
- `fut_completer` returns a pending future and a handle that any thread, including one running
  the library's own executor, completes exactly once. If the future is dropped first, the result
  is freed instead of stored. Handles are never reused, so a second completion is a fatal error,
  never a use of freed memory.

### The `velt_native` SDK

`crates/velt_native` (with `crates/velt_native_macros`) contains no runtime code: the table
type, safe wrappers for strings, byte arrays, arrays and handles, `#[derive(velt_native::Value)]`
for structs in Velt's layout, the `init!` helper and the `#[export]` macro that writes the
signature records and catches panics. It is tested against a mock table.

### Hot reload

Only vtables, future headers and per-server handler slots may store code addresses
([hot-reload.md](hot-reload.md)). Native libraries satisfy that rule because they are **loaded
once per dev host and never unloaded**: their code pointers, including `work` functions, stay
valid for the life of the host. When a library built from source changes (an author working on a
path dependency), the host restarts instead of swapping (`crates/veltc/src/dev/native.rs`). A
Velt edit in a program that uses native packages hot-swaps as usual, and state the library holds
survives the swap.

### Bundles

`velt native build [--target <triple>]` runs cargo in the crate directory and writes one
**bundle** per target: `native.json` (target, table version, export list with signatures,
system libraries), the shared library and, on Linux and macOS, one prelinked object.

A Rust staticlib carries its own copy of Rust's standard library; linked next to the runtime
(which carries another), their symbols clash. On Linux and macOS the build therefore prelinks
the staticlib into one relocatable object (`ld -r --force-group-allocation`, ld64 `-r` on macOS)
and hides every global except the package's exports and its init. The result cannot collide with
the runtime or another package. `--force-group-allocation` is required: without it the
runtime's copy of a section group (`DW.ref.rust_eh_personality`) wins over the library's and
leaves the library's localized references dangling. Windows has no partial link, so release
builds there link the DLL.

The crate's `[lib] name` must be `velt_native_<p>`, so the library files of two packages never
share a name (`-l` arguments, DLLs next to one executable). Tools can be overridden for cross
builds with `VELT_CARGO`, `VELT_NATIVE_LD` and `VELT_NATIVE_OBJCOPY`
(`crates/vpm/src/native/build.rs`).

### Registry, lockfile and cache

| Request | Meaning |
|---|---|
| `GET <url>/api/v1/<name>/<version>/native/<triple>` | download a bundle |
| `PUT <url>/api/v1/<name>/<version>/native/<triple>` | upload a bundle |

- The index entry of a version gains `native_abi` and `native`, a map from target to the
  bundle's checksum.
- Versions are immutable, with one exception: a target can be **added** to a published version.
  A target that exists is never replaced.
- `velt.lock.json` pins the checksum of **every** published target of each locked package, so
  the lockfile is identical on every OS and each machine verifies the bundle for its own target.
  Under `--locked`, a locked version keeps exactly its locked targets.
- Bundles are cached under `$VELT_HOME/cache/native/<name>-<version>/<triple>/` and verified
  against the lockfile in a staging directory before anything loads or links them.

`velt publish` builds the targets the host can build and takes the others from
`--native-artifacts <dir>`, so the intended workflow is a CI matrix with one runner per OS
followed by one publish. Publishing fails when a target listed in `native.targets` is missing.

### Trust

A native library is code that runs in every program that uses the package. A program allows it
explicitly in its manifest's `allowNative` list, by registry name or bound to a git repository or
URL prefix (`crates/vpm/src/manifest/allow_native.rs`); installing checks every package with
native code against the list. An update that changes a package's native code says so.

### Building from source

When the lockfile has no bundle for the user's target, vpm can build the crate from the cached
sources with cargo, using the published `Cargo.lock` (`--locked`). For registry packages this
requires `VELT_NATIVE_FROM_SOURCE=1`, because it runs the package's build scripts. Without a
bundle and without that build, installing fails with an error that names the published targets.
A path dependency on a package with native code builds its crate with cargo into the package's
`target/velt-native/`.

## Compilation

| Part | Where |
|---|---|
| Manifest, bundles, build, install, cache | `crates/vpm/src/manifest/`, `crates/vpm/src/native/`, `crates/vpm/src/install.rs` |
| Registry endpoints | `crates/velt_registry` |
| `declare` check against the export list | `crates/veltc/src/native/` |
| Init calls in `velt_main` | `crates/velt_vir` (`LowerOptions::native_inits`) |
| The function table | `crates/velt_rt/src/native/` |
| Linking (`NativeLink`, DLL placement, system libraries) | `crates/velt_link/src/native.rs` |
| Loading into the JIT host | `crates/veltc/src/dev/native.rs` |

A release object records the system libraries its crate needs (rustc's `native-static-libs`) in
the bundle, and `velt_link` adds them only to programs that use the package. End-to-end coverage
is `crates/veltc/tests/native_packages.rs`: an author publishes
`crates/veltc/tests/native_package` to a `velt registry serve`; a user without cargo adds it and
runs it with `velt run`, with
`velt build --release` (the executable still runs after the cache is deleted) and with
`velt dev` (a Velt edit hot-swaps and the library's state survives).

## Alternatives considered

**Linking runtime symbols by name.** Simpler for the library author, but it cannot work in the
JIT host or for a Windows DLL, and it offers no version check (see the function table above).

**Linking the Rust staticlib directly in release builds.** Two copies of Rust's standard
library in one link clash on symbols such as `rust_eh_personality` and allocator shims. The
prelinked, localized object avoids that on Linux and macOS.

**Building from source on every user's machine.** Requires a Rust toolchain for every user of
the package and runs build scripts on install. Prebuilt, checksummed bundles keep users
toolchain-free; building from source is an explicit opt-in.

## Limitations

- Packages with native code cannot target WebAssembly (`wasm: true` is rejected).
- Native code cannot call back into Velt: function types do not cross the boundary.
- A statically linked (musl) `velt` cannot load native libraries into its JIT host; `velt dev`
  says to use `velt dev --exe`.
- A remote publish uploads the package before its bundles, so for a moment the version is
  visible without libraries, and installing it then fails with the missing-library error.
