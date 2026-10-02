# Native libraries of packages — CONTRACT

How a package's Rust crate (`[native]` in [`velt.toml`](velt_toml.md)) talks to Velt code and to
the runtime, how its library is built, published, installed, linked and loaded. Design and
rationale: [native-packages.md](../design/native-packages.md). Implemented by `crates/velt_rt`
(`native.rs`), `crates/velt_native` (the SDK), `crates/vpm` (`native/`), `crates/velt_link`
(`NativeLink`), `crates/velt_vir` (`LowerOptions::native_inits`) and `crates/veltc`
(`native.rs`, `dev/native.rs`).

## Calls from Velt into the library

Velt declares the library's functions with `declare function` / `declare async function`, with
the pass modes of `rt_abi_async.md` §3.1. Only these types cross the boundary (v1):

| Velt | Parameter (C) | Result (C) | Signature name |
|---|---|---|---|
| `bool`, `i8`…`i64`, `u8`…`u64`, `f32`, `f64` (`number`) | by value | returned | `bool`, `i64`, `f64`, … |
| `string` | `const VeltStr*` (borrowed for the call) | trailing `VeltStr*` out-pointer | `string` |
| `u8[]` | `const VeltBytes*` (borrowed for the call) | trailing `VeltBytes*` out-pointer | `u8[]` |
| `IoResult<T>`, `T` one of the above | — | trailing `IoResult<T>*` (§3) | `IoResult<T>` |
| `IoStatus` | — | trailing `VeltErr*` | `IoStatus` |
| `void` | — | nothing | `void` |
| `Promise<R>` (`declare async function`) | — | returns `VeltFut*`, result `R` at +16 | the `R` name, `async ` before the signature |

`isize`/`usize`, structs, arrays other than `u8[]`, classes, closures and functions cannot cross:
native objects are `u64` handles (§3.2) that the package wraps in a class with
`[Symbol.dispose]()` (or a Copy struct for copies used by async calls).

**Signatures** are written `(<param>,<param>)-><result>`, with `async ` in front for
`declare async function`: `(string,u32)->IoResult<u64>`, `async (u64,string)->IoResult<string>`.

**Names**: every export of package `p` starts with `p_` (`-` in the package name becomes `_`),
plus `velt_native_init_p`. The compiler requires every `declare` in a package with a library
to name one of **its own** exports with **exactly** the recorded signature; no package may
declare another package's export, and a package with a library may not declare `velt_rt_*`
runtime functions (only std does). `IoResult`/`IoStatus` are std's `velt:io` types, identified
by definition (a look-alike struct is rejected). Violations are compile errors at the `declare`
(`veltc/src/native/`).

**Library name**: the crate's `[lib] name` is `velt_native_<p>`, so its files are
`libvelt_native_<p>.so`/`.dylib` and `velt_native_<p>.dll`; `velt native build` refuses any
other name (two packages' DLLs beside one executable must never collide).

**SDK parameters** are borrowed for the call only: `#[export]` rejects references with an
explicit lifetime (`&'static str`).

## Signature records

The SDK's `#[velt_native::export]` emits, for each exported function `f`, a data symbol
`velt_sig_f` holding the NUL-terminated signature of `f`. `velt native build` reads the shared
library's exports with the `object` crate (any target's ELF, Mach-O or PE) and writes the
`exports` table of `native.toml`. Building fails when the init function is missing, an export
lacks the `p_` prefix, an export has no record, or a record names nothing exported.

## The function table

```c
typedef struct VeltRtApi {
    uint32_t abi_version;                    // 1
    uint32_t size;                           // sizeof(VeltRtApi) of the runtime
    void (*str_new)(const uint8_t* p, size_t len, VeltStr* out);     // owned copy of UTF-8 bytes
    const uint8_t* (*str_bytes)(const VeltStr* s, size_t* len);      // a string's bytes (borrowed)
    void (*str_drop)(VeltStr* s);                                     // velt_rt_str_drop
    void (*bytes_new)(const uint8_t* p, size_t len, VeltBytes* out); // owned copy (rt allocator)
    void (*bytes_drop)(VeltBytes* b);                                 // velt_rt_bytes_drop
    const uint8_t* (*bytes_data)(const VeltBytes* b, size_t* len);    // a u8[]'s bytes (borrowed)
    VeltFut* (*fut_blocking)(void (*work)(void* ctx, void* out), void* ctx,
                             void (*drop_ctx)(void* ctx), size_t result_size,
                             void (*drop_result)(void* result));
    VeltFut* (*fut_completer)(size_t result_size, void (*drop_result)(void* result),
                              uint64_t* handle);
    void (*complete)(uint64_t handle, const void* result);
    _Noreturn void (*fatal)(const uint8_t* msg, size_t len);         // `panic: <msg>`, exit 101
} VeltRtApi;
```

- The library never links a `velt_rt_*` symbol: it gets this table (every entry is the runtime
  function or helper named in its comment). A library is `dlopen`ed into `velt dev`'s host,
  whose runtime symbols are not exported, and a DLL cannot import from an executable.
- **Versioning**: the table is append-only; `abi_version` grows by one with each addition. A
  library records the version it needs (`abi` in `native.toml`, `native_abi` in the index);
  vpm refuses a library needing more than the running `velt` provides
  (`vpm::native::NATIVE_ABI` = velt_rt's `NATIVE_ABI_VERSION`), and the SDK's init checks
  `abi_version` and `size` again.
- `fut_blocking`: a future whose first poll runs `work(ctx, out)` on the runtime's blocking
  pool; `work` writes `result_size` bytes (align ≤ 8) and owns `ctx` from then on. Dropped before
  its first poll, `drop_ctx(ctx)` runs instead; dropped while `work` runs, the finished result
  goes to `drop_result` (if not null).
- `fut_completer`: a pending future and a `handle` that `complete(handle, result)` completes
  exactly once, from any thread (`result_size` bytes are moved). If the future was dropped first,
  `drop_result` gets the result. Handles are ids in a runtime table, never reused: a second
  `complete` or an unknown id is a fatal error, never a use of freed memory. The SDK's
  `Completer` (results that can fail only) completes with the error "completer dropped" when it
  is dropped without completing, so awaiting Velt code never hangs.
- `VeltStr` and `VeltBytes` layouts are private to the runtime: libraries read them only through
  `str_bytes` and `bytes_data`. velt_rt's unit tests check that the SDK's `Api`, `VeltStr`,
  `VeltBytes`, `VeltErr` and `IoResultSlot` match the runtime's exactly.
- Hot-reload rule (`rt_abi_async.md` §13.5): the only code pointers stored are `work`,
  `drop_ctx` and `drop_result`, which point into the library; libraries are never unloaded. A
  library never stores a Velt code pointer (no function types cross the boundary).

## Start-up

`velt_main` (VIR lowering, `LowerOptions::native_inits`) begins, for each package with a library
in package-graph order, with

```c
int32_t rc = velt_native_init_<pkg>(velt_rt_native_api());
velt_rt_native_check(rc, &"<pkg>");   // rc != 0: "error: the native library of package `<pkg>` failed to start", exit 1
```

`velt_rt_native_api` and `velt_rt_native_check` are in [rt_abi.md](rt_abi.md). The SDK's init
(`velt_native::package!(p)`) stores the table and silences Rust's panic hook (exports catch
panics: `IoResult`/`IoStatus` results become `native panic: <msg>` errors, other results are
fatal).

## Bundles

A bundle is one target's library, a directory exchanged as a `VELTPKG1` archive
(`vpm::archive`) restricted to these paths:

```text
native.toml                   # package, version, target, abi, shared, import_lib?, static?, [exports]
shared/libvelt_native_<p>.so  # .dylib (install name @rpath/...) on macOS
shared/velt_native_<p>.dll    # Windows, with shared/velt_native_<p>.dll.lib
static/<p>.o                  # Linux and macOS only
```

- `velt native build [--target <triple>]` runs
  `cargo build --release --lib --target <triple> --manifest-path <pkg>/<path>/Cargo.toml`
  (`$VELT_CARGO`); the crate must build a `cdylib` and (except on Windows) a `staticlib`.
- **Prelinking** (`static/<p>.o`): `ld -r --force-group-allocation -u <export>… lib.a`, then
  `objcopy --strip-debug --keep-global-symbols=<exports + init>` (GNU ld/objcopy,
  `$VELT_NATIVE_LD`/`$VELT_NATIVE_OBJCOPY`); macOS: `ld -r -exported_symbols_list`. The Rust
  std inside it becomes local, so it cannot clash with the runtime's copy or another package's.
  Section groups are turned into plain sections first: otherwise the runtime's copy of a group
  (e.g. `DW.ref.rust_eh_personality`) would replace the library's and leave references dangling.
- `native.toml` may only name the bundle's own files: `shared` and `import_lib` under `shared/`,
  `static` under `static/`, no absolute paths or `..` (checked on unpack, publish and load).
- The checksum of a bundle is the content hash of its files (`vpm::native::bundle::checksum`,
  the hash `velt.lock` records for packages).

## Install

For the build target (`InstallOptions::target`):
- A **registry** package with `native` checksums in the lockfile: the bundle for the target is
  fetched into `<cache>/native/<name>-<version>/<triple>/`, after its checksum (and its
  `native.toml`'s package, version and target) is verified in a staging directory; nothing
  unverified is ever where the compiler loads or links it. A cached bundle whose files changed is
  replaced.
- No bundle for the target: built from the package's sources with `cargo build --locked` (the
  published `Cargo.lock`, which `velt publish` requires) only when cargo is installed **and** the
  user opted in with `VELT_NATIVE_FROM_SOURCE=1` (the build runs the package's build scripts);
  `velt` announces the build before it starts. Otherwise the error

  ```text
  `sqlite 0.1.0` has no prebuilt native library for <triple> (published: <targets>).
  Install Rust (https://rustup.rs) to build it from source, or ask the package author to publish this target.
  ```
- A **path** package (or the root package) with `[native]` is built from source into
  `<package>/target/velt-native/<triple>/` (cargo's work in `.../cargo`).
- `wasm32-*` targets: an error naming the package (WebAssembly libraries are not supported).
- The result is `vpm::GraphPackage::native: Option<NativeLib>`; `velt add`/`velt install` list
  every package that runs native code and whether it was prebuilt or built from source.

## Linking (`velt_link::NativeLink`)

| Build | Linux / macOS | Windows |
|---|---|---|
| debug | `-L<dir> -lvelt_native_<p> -Wl,-rpath,<dir>` | import library; the DLL is copied next to the executable |
| `--release` | `static/<p>.o` after the program's objects: self-contained | as debug (no partial link with MSVC) |
| `velt dev` (JIT) | `dlopen` (RTLD_NOW, RTLD_LOCAL), exports and init given to the JIT | `LoadLibraryW` |

The link stamp hashes every native file (path, size, modification time). A statically linked
(musl) `velt` cannot load libraries into its JIT host and says to use `velt dev --exe`.
`velt dev` watches the crate sources of libraries built from source and restarts (never swaps)
when they change.
