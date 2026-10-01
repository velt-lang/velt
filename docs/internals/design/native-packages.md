# Design: native code in packages

Status: implemented as a prototype ([#22](https://github.com/velt-lang/velt/issues/22)); the
contract is [native_abi.md](../contracts/native_abi.md). The sections below are the proposal;
[Decisions](#decisions) and [As built](#as-built) record what changed.

A package can ship a Rust crate (`native/`) next to its Velt sources. Database drivers, image
codecs and crypto then live in packages instead of `velt_rt`. **Users never need cargo or a Rust
toolchain.** Installing such a package downloads a **prebuilt binary for the user's target**
from the registry and caches it, in the style of npm prebuilt addons and esbuild's per-platform
packages. Only package authors build, when they publish. Most packages are pure Velt and never
touch any of this.

How a program uses the binary depends on the mode:

| Mode | How the native library is used |
|---|---|
| `velt dev` (JIT host) | loaded into the running program (`dlopen` / `LoadLibrary`), nothing linked |
| `velt build` / `velt run` (debug) | linked as a shared library, the way debug builds use the shared runtime |
| `velt build --release` | linked statically, so the executable stays self-contained |

## Starting point

- Packages hold only `velt.toml` and `src/**`. Both `vpm::contents` and `vpm::archive::check_path`
  reject anything else.
- `declare function` already declares a C-ABI symbol. The parser produces `ItemKind::ExternFn`,
  which becomes `hir::Def::ExternFn` and then `vir::ExternFn` (`docs/reference/modules.md`). User
  modules may use it, but every symbol must come from `velt_rt`:
  - `velt_link::LinkRequest` takes only objects plus one runtime library.
  - The dev JIT resolves only `velt_rt_host::abi_symbols::symbol_table()` and `c_symbols`.
- This is why the sqlite, postgres and redis drivers are built into `velt_rt`
  (`rt_abi_async.md` §14). The roadmap wants them moved out into versioned packages.

## Package layout

```text
sqlite/
  velt.toml
  src/lib.vlt          # the Velt API; `declare function sqlite_open(...)` etc.
  native/Cargo.toml    # crate-type = ["cdylib", "staticlib"]; depends on `velt_native`
  native/src/lib.rs
```

## Manifest: the `[native]` table

This addition to [`velt.toml`](../contracts/velt_toml.md) is parsed by `vpm::manifest`:

```toml
[native]
path = "native"            # the Cargo crate directory (default "native")
targets = [                # what `velt publish` builds or collects; it publishes nothing less
  "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
  "x86_64-apple-darwin", "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
]
wasm = false               # true: the author also ships a wasm32-wasip1 static object
```

Validation rules:
- The `path` directory contains a `Cargo.toml`.
- Every entry in `targets` is a triple Velt supports.
- No package may declare `[native]` when its name collides with another package's
  [export prefix](#exported-symbol-names) after `-` becomes `_`.

## ABI between Velt and the native crate

The contract will be a new file, `docs/internals/contracts/native_abi.md`.

### Calls from Velt into native code

Velt calls native code through the existing `declare function` and `declare async function`. The
pass modes are the ones in `rt_abi.md` and `rt_abi_async.md` §3:
- Scalars go by value.
- Strings, arrays, structs and tuples go as a read-only borrowed pointer.
- A non-scalar result goes through a trailing out-pointer, and the function returns `void`.
- Errors use `IoResult<T>` / `IoStatus`.
- Native objects are `u64` handles, with 0 meaning none (§3.2).

Nothing changes in the parser, HIR or VIR.

```ts planned
// sqlite/src/lib.vlt
declare function sqlite_open(path: string, flags: u32): IoResult<u64>;
declare async function sqlite_query(db: u64, sql: string): Promise<IoResult<Rows>>;
```

### Exported symbol names

Every symbol the crate exports starts with `<pkg>_`, where `<pkg>` is the package name with `-`
turned into `_`. The one exception is `velt_native_init`. The rule is checked in two places:

- **At publish:** `velt publish` reads the export list of the built library and refuses
  unprefixed exports. The list is stored in the bundle's `native.toml`.
- **In sema:** a `declare` in a package module must name either an export of that package's own
  native library or a `velt_rt_*` symbol from std's set. Anything else is a diagnostic at the
  `declare` ("`sqlite_opne` is not exported by the native library of `sqlite 1.2.0`"), not a
  link error later.

A package may only declare its own exports. Two packages therefore can't bind each other's native
code, and the symbols of any two packages never collide.

### How native code reaches the runtime: a versioned function table

The native crate **never links `velt_rt_*` by symbol name.** It exports one entry point:

```rust
#[no_mangle]
pub extern "C" fn velt_native_init(api: *const VeltRtApi) -> i32; // 0 = ok
```

The runtime calls it once, before any of the crate's exports run. `VeltRtApi` is a
`#[repr(C)]` table:

```rust
#[repr(C)]
pub struct VeltRtApi {
    pub abi_version: u32,   // bumped when a field is added
    pub size: u32,          // size_of::<VeltRtApi>() of the runtime that built it
    pub str_from_utf8: unsafe extern "C" fn(*const u8, usize, *mut VeltStr),
    pub str_clone: unsafe extern "C" fn(*const VeltStr, *mut VeltStr),
    pub str_drop: unsafe extern "C" fn(*mut VeltStr),
    pub bytes_alloc: unsafe extern "C" fn(usize, *mut VeltBytes),
    pub err_new: unsafe extern "C" fn(i32, *const u8, usize, *mut VeltErr),
    pub fut_new_blocking: /* see "Async" */,
    pub fut_completer: /* see "Async" */,
    // ... append-only
}
```

Every field is a documented `velt_rt_*` function: the entry in `native_abi.md` names the symbol
and points to its `rt_abi*.md` section. So native code still reaches the runtime only through
documented `velt_rt_*` functions, but it receives them as pointers rather than linking them.

A table is used rather than symbol imports because it is the only scheme that works the same in
all three modes:
- A library `dlopen`ed into `velt` (the JIT host) can't see the host's statically linked runtime
  symbols. `velt` doesn't export them dynamically.
- A Windows DLL can't import from `velt.exe` without an import library for it.
- A release executable has no shared runtime to link against.
- It also gives native code a version check before its first call.

### Versioning

- `abi_version` grows by one whenever fields are appended. Fields are never removed or reordered.
  Existing behaviour is frozen in the same way as the `[M3/M4 — frozen]` sections of `rt_abi.md`.
- A crate records the minimum `abi_version` it needs (`velt_native` writes it into `native.toml`,
  which becomes `native_abi` in the registry index).
- `velt_native_init` returns an error code when `api.abi_version` or `api.size` is too small.
- vpm checks `native_abi` against the running `velt` before downloading anything: "`sqlite 1.2.0`
  needs Velt native ABI 3; this velt has 2. Update velt."
- Package versions follow semver as usual. The native binaries are part of the package version
  ([Registry](#registry-and-versioning)).

### Who calls `velt_native_init`

- **JIT (`velt dev`):** the host calls each package's init right after loading the library, then
  passes the library's exports to `JITBuilder::symbol` using `dlsym` / `GetProcAddress` on the
  export list.
- **Executables (debug and release):** the driver writes a small `natives.o` with
  cranelift-object, the same way it writes `emit_entry_object`. It defines
  `velt_native_inits: [Option<extern "C" fn(*const VeltRtApi) -> i32>; N+1]`, terminated by a
  null entry.
  - `velt_rt`'s start-up walks the array before `velt_main` and reports a failing init as a
    start-up error that names the package.
  - Programs without native packages get an array holding only the terminator.
  - This is an additive `rt_abi.md` change: one new symbol that the runtime references.

### Memory and ownership

- Strings, bytes and arrays that cross into Velt are allocated **through the table**: the runtime
  allocator, so generated code can adopt them (`rt_abi_async.md` §4).
- Arguments are borrowed for the duration of the call. Native code that keeps a string clones it
  with `str_clone`.
- Native-owned objects live behind `u64` handles. They follow the two handle kinds of §3.2: copy
  structs with generation checks, or class-owned handles released by `dispose()`.
- A panic must not unwind across `extern "C"`. `velt_native`'s export helper catches it and turns
  it into an `IoResult` error, or aborts with the package name for functions that can't fail.

### Async

The native crate has its own copy of Rust std. It can't share `velt_rt`'s tokio runtime. The table
offers two ways to produce a `VeltFut*` that the runtime polls, following the protocol of
`rt_abi_async.md` §1–§2:

- `fut_new_blocking(work, ctx, drop_ctx, result_size) -> *mut VeltFut`. The runtime runs
  `work(ctx, out)` on its blocking pool and completes the future with the result written to `out`.
  This fits sqlite and most C-library wrappers.
- `fut_completer(result_size, out_handle) -> *mut VeltFut`. This returns a pending future and a
  completion handle. Any native thread, including one running the crate's own tokio runtime (for
  example a postgres client), calls `api.complete(handle, result)` once.
  - Dropping the future (cancellation) marks the handle as cancelled. A later `complete` then
    frees the result instead of storing it.

Both keep the code pointers in the native library, which is never unloaded (see the next
section). No Velt code pointer is involved.

### The `velt_native` SDK crate

`crates/velt_native` is published to crates.io with its version tied to `abi_version`. It contains
no runtime code: only `VeltRtApi`, safe wrappers (`VeltStr`, `VeltBytes`, `IoResult<T>`, handle
tables), the `init!` helper that stores the table, and `#[export]`-style helpers that catch panics.
It is tested against a mock table.

## Hot reload

The rule from `hot-reload.md` decision 3 and `rt_abi_async.md` §13.5 is that only vtables, future
headers and per-server handler slots may store code addresses.

- **Native libraries are loaded once per dev host and never unloaded or swapped.** Their code
  pointers, including the `work` functions above, stay valid for the whole life of the host, so
  storing them is safe.
- When a native library changes, the host restarts instead of hot-swapping. This only happens to
  authors, through a path dependency whose `native/` changed; the supervisor watches that
  directory.
- **Native code must never store a Velt code pointer.** In v1 this is enforced by construction:
  sema rejects function-typed parameters on `declare` outside std.
- Callbacks are planned for later. They will go through runtime-owned callback slots, read again
  on every call and replaced on a swap, which is the `VeltHandler` pattern of §13.5.

## Artifacts per target

`velt publish` produces one **native bundle** per target: an archive in the existing `VELTPKG1`
format, with a different allowed-path set.

```text
native.toml            # target, abi_version, crate version, export list
shared/libsqlite.so    # or .dylib, or sqlite.dll plus sqlite.dll.lib
static/sqlite.o        # one prelinked relocatable object
```

- **Why a prelinked object instead of the staticlib.** A Rust `staticlib` carries its own copy of
  Rust std. Linked next to `libvelt_rt.a`, the shared symbols clash: `rust_eh_personality`,
  allocator shims, std internals.
  - On the author's machine, `velt publish` runs `ld -r` (ELF) or `ld64 -r` (Mach-O) over the
    staticlib.
  - It then hides every global except `velt_native_init` and `<pkg>_*`, using
    `--version-script` / `-exported_symbols_list` or `objcopy --localize-hidden`.
  - The result is a single object that can't collide with `velt_rt` or another package.
- **Windows** has no partial link. The prototype checks whether the MSVC staticlib links cleanly
  next to `velt_rt.lib`.
  - If not, Windows `--release` places the DLL next to the executable instead, as the debug build
    already does. This is the one open question below.
- **wasm:**
  - With `wasm = false`, building for `wasm32-*` fails with a diagnostic that names the package.
  - With `wasm = true`, the author adds a `wasm32-wasip1` bundle holding only `static/`, which is
    linked like the release path. wasm has no `dlopen`, and the browser flavor isn't supported in
    v1.

## Registry and versioning

These additions extend the remote protocol in `velt_toml.md`:

| Request | Meaning |
|---|---|
| `GET <url>/api/v1/<name>/<version>/native/<triple>` | download a bundle |
| `PUT <url>/api/v1/<name>/<version>/native/<triple>` | upload a bundle (`X-Velt-Checksum`, bearer token) |

The index entry gains two fields:

```toml
[[version]]
version = "1.2.0"
checksum = "sha256:…"            # the source archive, as today
native_abi = 3
native = { "x86_64-unknown-linux-gnu" = "sha256:…", "aarch64-apple-darwin" = "sha256:…" }
```

- Versions stay immutable. The single exception is that a **target can be added** to a published
  version, for example when a new platform arrives later. A target that already exists is never
  replaced.
- The source archive now also includes `native/**`, minus `target/`, because the fallback build
  needs it.
- `velt.lock` records the checksum of **every** published target of each locked package. The
  lockfile is then identical on every OS, and a CI machine verifies the bundle for its own target.
- Bundles are cached under `~/.velt/cache/native/<name>-<version>/<triple>/`. A cached bundle is
  reused when its checksum matches the lock and replaced when it is tampered with, just like source
  packages in `vpm::cache`.

## Building

- **Authors.** `velt native build [--target <triple>]` runs
  `cargo build --release --target <triple>` in `native/`, then prelinks and writes the bundle to
  `target/velt-native/<triple>/`.
  - `velt publish` builds what the host can build and collects the other targets with
    `--native-artifacts <dir>`. The intended workflow is a CI matrix with one runner per OS that
    uploads its bundles, followed by one publish.
  - Publishing fails if any target listed in `[native] targets` is missing. Targets can still be
    added later.
- **Users, fallback.** If the lock has no bundle for the user's target, vpm builds from the cached
  sources with the same cargo invocation, when `cargo` is on PATH. Without cargo:

  ```text
  error: `sqlite 1.2.0` has no prebuilt binary for aarch64-unknown-linux-musl
         (published: x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu, ...).
         Install Rust (https://rustup.rs) to build it from source, or ask the package author
         to publish this target.
  ```
- **Trust.** A native bundle is code that runs in every program that uses the package. `velt add` and
  `velt install` print once which packages carry native code. The checksums in `velt.lock` pin the
  exact binaries.

## Linking

`velt_link::LinkRequest` gains `native: &[NativeLink { shared: PathBuf, static_obj: PathBuf }]`,
and the link stamp (`veltc/src/link.rs::link_key`) hashes the bundles.

| Mode | What is linked |
|---|---|
| Debug, Unix | `-L<dir> -l<pkg> -Wl,-rpath,<dir>` per package, as with `velt_rt_shared` |
| Debug, Windows | link against the `.dll.lib`; `place_dll` copies the DLL next to the exe |
| Release | append `static/<pkg>.o` after the objects and before the runtime library |
| JIT | nothing linked; `dlopen` and `JITBuilder::symbol` as above. A static musl `velt` has no loader, so it says to use `velt dev --exe` |

## Implementation plan

The prototype driver is **sqlite**:
- It is synchronous, bundles its C library through rusqlite, needs no server, and exercises
  handles and `fut_new_blocking`.
- `packages/sqlite` mirrors `std/sqlite.vlt` with `sqlite_*` exports.
- `std/sqlite` stays until the drivers are migrated as a follow-up.

Each step below lands with its own tests:

1. **vpm and registry** (`crates/vpm`, `crates/velt_registry`):
   - parse `[native]`
   - allow `native/**` in archives
   - add the index `native` and `native_abi` fields, the bundle endpoints, the lockfile checksums
     and `cache::fetch_native`
   - add `GraphPackage.native` with exports and paths (a `velt_toml.md` contract update)
2. **Author tooling** (`veltc`): `velt native build`, `velt publish --native-artifacts`, the
   prelink step, the cargo fallback and its message, and a `cli.md` update.
3. **Runtime table** (`crates/velt_rt`):
   - `VELT_RT_API`, `fut_new_blocking`, `fut_completer`, `complete`
   - the `velt_native_inits` walk at start-up
   - `native_abi.md`, and a test in the style of `abi_symbols` that checks the table against the
     doc
4. **SDK** (`crates/velt_native`), unit-tested against a mock table.
5. **Linking** (`crates/velt_link`, `veltc/src/link.rs`): `NativeLink`, shared and static
   arguments, `natives.o`, and the link stamp.
6. **Dev JIT** (`veltc/src/dev`, `velt_codegen_cl/src/dev/version.rs`): load the library, init it,
   register the symbols, and restart on a native change.
7. **Sema** (`velt_sema/src/collect/declare.rs`): check the export list, reject function-typed
   parameters, and diagnose a wasm target without a bundle.
8. **`packages/sqlite`.** An end-to-end test against a local `velt registry serve`, with no cargo
   on PATH:
   - `velt add sqlite`
   - `velt run`
   - `velt build --release`: the executable has no dynamic dependency on the library
   - `velt dev`, where a Velt edit still hot-swaps
9. **Docs:** `docs/tooling/packages.md` and `manifest.md`, and a book page "Writing a package with
   native code".

## Decisions

1. **Windows `--release` links the DLL** (next to the executable, as debug builds do): two Rust
   staticlibs in one MSVC link collide on std symbols and MSVC has no `-r`. Linux and macOS
   keep the prelinked object. Revisit if a clean static path on MSVC appears.
2. **Signatures are required**, recorded by the SDK's `#[export]` (not written by hand) and
   checked exactly for every `declare` of the package: a mismatch is a compile error.
3. **Cache**: native bundles live in vpm's package cache, `~/.velt/cache/native/...`
   (`$VELT_HOME/cache`).
4. **Handles** surface in the package's Velt API as classes with `[Symbol.dispose]()` (and
   `using`), plus a Copy reference type for async calls (`packages/sqlite`).
5. **Integrity**: bundles are verified against `velt.lock` in a staging directory before they are
   placed where anything loads or links them; `velt add`/`velt install` list the packages that
   run native code.

## As built

Differences from the proposal above:

- **Init per package**: `velt_native_init_<pkg>` (one global `velt_native_init` would collide in
  a static link). `velt_main` calls each package's init through VIR lowering
  (`LowerOptions::native_inits`) with `velt_rt_native_api()` and checks the result with
  `velt_rt_native_check`; there is no `natives.o` and no symbol the runtime must find. The JIT
  host resolves the init like any export, so all three modes start the same way.
- **The table** (`VeltRtApi`, version 1): `str_new`, `str_bytes`, `str_drop`, `bytes_new`,
  `bytes_drop`, `fut_blocking`, `fut_completer`, `complete`, `fatal`. `str_bytes` keeps the
  string layout private to the runtime; errors are built by the SDK from `str_new`.
- **The check** runs in the driver after sema (`veltc/src/native.rs`), where the package graph
  is known, rather than in `velt_sema`; HIR is unchanged. Only scalars, `string`, `u8[]`,
  `IoResult<T>`/`IoStatus` and `void` cross the boundary, so function types are rejected by
  construction.
- **Prelinking** needs `ld -r --force-group-allocation`: without it the runtime's copy of a
  section group (`DW.ref.rust_eh_personality`) wins over the library's and leaves the
  library's now-local references dangling.
- **Lockfile** `native` is a `[package.native]` subtable. Under `--locked` a locked version keeps
  exactly its locked targets (a target the author added later does not fail CI).
- **Library names**: the crate's `[lib] name` must be `velt_native_<pkg>`, so files never clash
  (`-l` names, DLLs next to an executable). `native.toml` may only name the bundle's own files.
- **Builds from source** use the published `Cargo.lock` (`--locked`) and need
  `VELT_NATIVE_FROM_SOURCE=1` for registry packages, since they run the package's build scripts.
- **Prototype**: `packages/sqlite` (rusqlite; rows as JSON decoded with `JSON.parse<T[]>`),
  tested end to end in `crates/veltc/tests/native_packages.rs`: publish to a
  `velt registry serve`, then without cargo `velt add`, `velt run`, `velt build --release` (runs
  with the cache removed) and `velt dev` (a Velt edit hot-swaps; the database opened before the
  swap survives).

## Known gaps

- Verified on Linux x86_64 only. The macOS prelink (`ld -r -exported_symbols_list`) and
  `install_name_tool` step, and Windows DLL placement, are written but untested.
- `velt_native` is not on crates.io; a published package's crate cannot yet build from source on
  a user's machine (the fallback) unless it depends on the SDK by git.
- `[native] wasm = true` is rejected; WebAssembly targets with native packages are an error.
- A remote publish uploads the package before its bundles: for a moment the version is visible
  without libraries (users then get the "no prebuilt native library" error).
- Callbacks from native code into Velt are not supported (planned: runtime-owned callback slots,
  the `VeltHandler` pattern).
- `std/sqlite` still lives in the runtime; migrating the drivers to packages is a follow-up.
