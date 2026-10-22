# Native libraries of packages — CONTRACT

How a package's Rust crate (`native` in [`package.vlt`](manifest.md)) talks to Velt code and to
the runtime, how its library is built, published, installed, linked and loaded. Design and
rationale: [native-packages.md](../design/native-packages.md). Implemented by `crates/velt_rt`
(`native.rs`), `crates/velt_native` (the SDK), `crates/vpm` (`native/`), `crates/velt_link`
(`NativeLink`), `crates/velt_vir` (`LowerOptions::native_inits`) and `crates/veltc`
(`native.rs`, `dev/native.rs`).

## Calls from Velt into the library

Velt declares the library's functions with `declare function` / `declare async function`, with
the pass modes of `rt_abi_async.md` §3.1. A package passes the same values std's runtime
functions do, under one rule for both (`veltc/src/native/`):

| Velt | Parameter (C) | Result (C) | Signature name |
|---|---|---|---|
| `bool`, `i8`…`i64`, `u8`…`u64`, `f32`, `f64` (`number`) | by value | returned | `bool`, `i64`, `f64`, … |
| `string` | `const VeltStr*` (borrowed for the call) | trailing `VeltStr*` out-pointer | `string` |
| `T[]`, `T` a value | `const VeltArray*`: `{ T* ptr; u64 len; u64 cap; }` (borrowed) | trailing `VeltArray*` out-pointer (elements from `alloc`) | `u8[]`, `{id:i64,name:string}[]`, … |
| a `struct` of values | `const S*` (borrowed) | trailing `S*` out-pointer | its shape: `{id:i64,name:string}` |
| `IoResult<T>`, `T` a value | — | trailing `IoResult<T>*` (§3) | `IoResult<T>` |
| `IoStatus` | — | trailing `VeltErr*` | `IoStatus` |
| `void` | — | nothing | `void` |
| `Promise<R>` (`declare async function`) | — | returns `VeltFut*`, result `R` at +16 | the `R` name, `async ` before the signature |

A value is a scalar, a `string`, an array of values or a `struct` of values, in Velt's own
layout (rt_abi.md): a struct is named by its shape, its fields' names (as Velt spells them),
order and types, so a field that differs is a build error. `isize`/`usize`, classes,
interfaces, unions, optional or private fields, recursive structs, closures and functions
cannot cross: native objects are `u64` handles (§3.2) that the package wraps in a class with
`[Symbol.dispose]()` (or a Copy struct for copies used by async calls). The SDK's
`#[derive(velt_native::Value)]` gives a `#[repr(C)]` Rust struct Velt's layout and its
signature name (field names camelCased, or `#[velt(name)]`); `velt_native::Array<T>` is a
Velt array in the runtime's memory.

**Signatures** are written `(<param>,<param>)-><result>`, with `async ` in front for
`declare async function`: `(string,u32)->IoResult<u64>`, `async (u64,string)->IoResult<string>`.

**Names**: every export of package `p` starts with `velt_p__` (`-` in the package name becomes
`_`, then a double underscore), plus `velt_native_init_p`. The prefix is structural: no C
library or system function starts with `velt_`, so a package's exports can't take the name of
one. It is one-to-one: package names separate words with single `-` or `_` (never doubled,
mixed or at the end), so `velt_a__` is never the start of `velt_a_b__`; and names that differ
only in `-` versus `_` count as the same, so the registry refuses `my_pkg` when `my-pkg` exists
and installing refuses two packages with native code whose names normalise the same. Names whose
first component is `rt`, `sig` or `native` (`rt`, `rt-str`, `native_init`) are reserved, since
`velt_rt_`, `velt_sig_` and `velt_native_` are Velt's own runtime functions, signature records
and init functions. Outside std, the compiler requires every `declare` to name one of
the exports of **its own** package's library with **exactly** the recorded signature: a root
program, a package without a library (`free`, `memcpy`) and another package's export are
errors. `IoResult`/`IoStatus` are std's `velt:io` types, identified
by definition (a look-alike struct is rejected). Violations are compile errors at the `declare`
(`veltc/src/native/`). Separately, sema rejects a `velt_rt_*` runtime function declared outside
std, in any package or the root program (`velt_sema` `collect/declare.rs`).

**Library name**: the crate's `[lib] name` is `velt_native_<p>`, so its files are
`libvelt_native_<p>.so`/`.dylib` and `velt_native_<p>.dll`; `velt native build` refuses any
other name (two packages' DLLs beside one executable must never collide).

**SDK parameters** are borrowed for the call only: `#[export]` rejects references with an
explicit lifetime (`&'static str`).

## Signature records

The SDK's `#[velt_native::export]` emits, for each exported function `f`, a data symbol
`velt_sig_f` holding the NUL-terminated signature of `f`. `velt native build` reads the shared
library's exports with the `object` crate (any target's ELF, Mach-O or PE) and writes the
`exports` table of `native.json`. Building fails when the init function is missing, an export
lacks the `velt_p__` prefix (an export with an older prefix gets its new name as a fix-it), an
export has no record, or a record names nothing exported.

## The function table

```c
typedef struct VeltRtApi {
    uint32_t abi_version;                    // 1
    uint32_t size;                           // sizeof(VeltRtApi) of the runtime
    void (*str_new)(const uint8_t* p, size_t len, VeltStr* out);     // owned copy, decoded lossily
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

- `str_new` decodes the bytes as UTF-8 from outside the program: an invalid sequence becomes
  U+FFFD by the WHATWG rule (Rust's `String::from_utf8_lossy`), and that includes a surrogate
  encoded on its own (`ED A0..BF xx`), so `ED A0 BD ED B8 80` gives six U+FFFD and two halves of
  a pair are never joined. The result is a canonical string (rt_abi.md "Strings") with its UTF-16
  length counted.
- `str_bytes` lends a string's bytes as they are stored: UTF-8 when the string is well-formed,
  else WTF-8 with lone surrogates (`ED A0..BF xx`, #377). The SDK's `str_of` gives a
  `Cow<str>`: borrowed when the bytes are UTF-8, else a copy with one U+FFFD per lone surrogate
  (the rule of every other output, so the length is unchanged). A `&str` parameter of an
  exported function borrows such a copy from the generated wrapper for the call; `String` and
  `Cow<str>` parameters take it as they are. The table and its version are unchanged: a library
  built with an older SDK stops the program (`fatal`) when it is given an ill-formed string, as
  it always did.
- The library never links a `velt_rt_*` symbol: it gets this table (every entry is the runtime
  function or helper named in its comment). A library is `dlopen`ed into `velt dev`'s host,
  whose runtime symbols are not exported, and a DLL cannot import from an executable.
- **Versioning**: the table is append-only; `abi_version` grows by one with each addition. A
  library records the version it needs (`abi` in `native.json`, `native_abi` in the index);
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

## Async I/O

The table's second part lets a library run async Rust on the program's scheduler, the way the
runtime's own drivers do: its futures are polled inline by the task awaiting them, its sockets
are the runtime's (on Velt's scheduler, registered with the worker's reactor; on tokio's, with
tokio's), and a task it spawns runs on the worker that spawned it. Nothing Rust-specific
crosses: a library carries its own std, possibly from another compiler.

```c
typedef uint32_t (*VeltPollFn)(void* state, void* cx, void* out);   // READY = 1, PENDING = 0
typedef struct { const void* (*clone)(const void*); void (*wake)(const void*);
                 void (*wake_by_ref)(const void*); void (*drop)(const void*); } VeltForeignWaker;
typedef struct { const void* runtime; const void* data; const VeltForeignWaker* vtable; } VeltWakerArg;

    VeltFut* (*fut_poll)(VeltPollFn poll, void* state, void (*drop_state)(void*), size_t result_size);
    const void* (*cx_waker)(void* cx);                // the task's waker, borrowed for the poll
    void* (*waker_clone)(const void* w);              // an owned (boxed) copy of a runtime waker
    void (*waker_wake)(void* w);                      // wake and free
    void (*waker_wake_by_ref)(const void* w);
    void (*waker_drop)(void* w);
    uint32_t (*fut_poll_ext)(VeltFut* f, const VeltWakerArg* w);  // await a runtime future
    void (*fut_drop)(VeltFut* f);
    void (*spawn)(VeltFut* f);                        // detached; result size 0
    VeltFut* (*tcp_connect)(const uint8_t* addr, size_t len);     // "host:port" -> IoResult<u64>
    int32_t (*stream_from_socket)(int64_t socket, uint64_t* out); // adopt a connected socket
    uint32_t (*stream_poll_read)(uint64_t h, const VeltWakerArg* w, uint8_t* buf, size_t cap,
                                 size_t* n, int32_t* err);        // n == 0: end of stream
    uint32_t (*stream_poll_write)(uint64_t h, const VeltWakerArg* w, const uint8_t* buf,
                                  size_t len, size_t* n, int32_t* err);
    uint32_t (*stream_poll_flush)(uint64_t h, const VeltWakerArg* w, int32_t* err);
    uint32_t (*stream_poll_shutdown)(uint64_t h, const VeltWakerArg* w, int32_t* err);
    int64_t (*stream_socket)(uint64_t h);             // for socket options; the stream owns it
    void (*stream_close)(uint64_t h);
    uint64_t (*now_ns)(void);                         // monotonic
    void* (*sleep_new)(uint64_t deadline_ns);
    uint32_t (*sleep_poll)(void* s, const VeltWakerArg* w);
    void (*sleep_reset)(void* s, uint64_t deadline_ns);
    void (*sleep_drop)(void* s);
    int64_t (*worker_index)(void);                    // -1 off the workers / on tokio's stealing
```

- `fut_poll` makes a `VeltFut` from a library future: the runtime calls `poll(state, cx, out)`
  on the thread of the awaiting task; `READY` means `result_size` bytes are at `out`. `state`
  is freed with `drop_state` when the future finishes or is dropped. A panic must not cross
  `poll` (the SDK turns it into the result's error).
- `cx` is the runtime's poll context, valid during the call; `cx_waker(cx)` is the task's
  waker, borrowed for the call. Functions that wait take a `VeltWakerArg`: `runtime` set to a
  runtime waker (borrowed or owned) when the library's waker is one, which costs nothing; else
  `data` (borrowed for the call) and `vtable`, the library's own waker (a combinator's), which
  the runtime clones through `clone` when it must keep it.
- Error codes (`err`, `stream_from_socket`): 0 is success; a positive value is the OS error
  number, a negative one the negated `IoError` code (rt_abi_async.md §3).
- Handles (streams, sleeps, owned wakers) are pointers the library owns until it closes,
  drops or wakes them; using one after that is undefined, as with any Rust value.
- The SDK wraps all of it: `#[export(async)] async fn`, `velt_native::rt::spawn`,
  `velt_native::net::TcpStream` (tokio's `AsyncRead`/`AsyncWrite` with the `tokio` feature),
  `velt_native::time::{sleep, timeout}`, `velt_native::rt::worker_index`.

## Strings

A Velt string knows its UTF-16 length and whether it is ASCII (rt_abi.md "Strings"), so
making one reads its text once. The table lets a library make strings without copying them:

```c
    void* (*alloc)(uint64_t size, uint64_t align);                  // the runtime's allocator
    void* (*realloc)(void* p, uint64_t old_size, uint64_t align, uint64_t new_size);
    void (*free)(void* p, uint64_t size, uint64_t align);
    void (*str_from_utf8)(const uint8_t* p, size_t len, VeltStr* out); // trusted UTF-8, counted
    void (*str_retain)(const VeltStr* s, VeltStr* out);               // another reference
    uint8_t* (*strbuf_spare)(VeltStr* buf, uint64_t min, uint64_t* avail);
    void (*strbuf_commit)(VeltStr* buf, uint64_t n, uint8_t ascii);
```

- `alloc`/`realloc`/`free`: memory Velt frees (arrays a library returns).
- `str_from_utf8`: like `str_new` for bytes the library knows are UTF-8 (no check in release
  builds; debug runtimes check).
- `str_retain`: another reference to the same immutable string (a count, no copy): a library
  keeps a string argument past the call this way (an async export's parameter).
- `strbuf_spare(buf, min, avail)`: makes `buf` a unique heap buffer with room for at least `min`
  bytes after its text and returns where the room starts (`*avail` bytes). Bytes written there
  are not text yet; the runtime keeps them when a later `strbuf_spare` grows the buffer.
  `strbuf_commit(buf, n, ascii)` makes the next `n` written bytes text: UTF-8 of whole
  characters, counted once (not at all when `ascii` is 1). The first non-ASCII text of an ASCII
  buffer gives it a header (rt_abi.md "Strings"): the text moves once, into a fresh buffer.
- The SDK's `velt_native::StrBuf` is built on them: it writes into the spare room directly (a
  bounds check and a copy per write, no call into the runtime), grows by doubling, keeps up to 23
  bytes to itself (an inline string), and `finish()` commits once. `velt_text`'s encoders
  (numbers, JSON, database rows) write into any `velt_text::Out`, a `Vec<u8>` or a `StrBuf`.
  `velt_native::Str` is an owned Velt string: a parameter (retained, not copied), a result
  (moved), an array element or a field.

What a string result costs, per path (`cargo run --release -p velt_rt --example
native_strings`, Apple M-series): a `StrBuf` is as fast as or faster than a `Vec<u8>` copied
into a string once (the runtime's own drivers before), from −11 % (short text) to +1 % (1 MB of
non-ASCII text), and 18–77 % faster than a `String` result (checked, then copied); a retained
`Str` parameter costs 2–4 ns where a `String` copy costs 13 ns (16 bytes) to 17 µs (64 KB).

## Drivers in the runtime

std's drivers (`velt:postgres`: crate `velt_std_postgres`) are crates on this SDK only
(`velt_native`, `velt_text`), linked into the runtime library instead of a package's own: their
`#[export]` functions are std's `declare`s (named `velt_std_<driver>__…`, checked against their
signature records like any package's, `veltc/src/native/` `check_std_driver`), and an
executable stays self-contained. The runtime hands them its table at start-up
(`velt_rt::native::embed`, from `entry::init`), as `velt_native_init_<pkg>` does for a
package, without replacing the panic hook (they share the runtime's std).

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
native.json                   # package, version, target, abi, shared, import_lib?, static?, exports
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
- `native.json` may only name the bundle's own files: `shared` and `import_lib` under `shared/`,
  `static` under `static/`, no absolute paths or `..` (checked on unpack, publish and load).
- A prebuilt bundle's `exports` list is not trusted as written. On unpack, publish and load:
  - every listed name must start with `velt_<pkg>__`;
  - the shared library's exports, read back with `vpm::native::exports::read`, must equal the
    list, signatures included. They are read from the ELF `.dynsym` section (every defined
    symbol with global, weak or unique binding and default or protected visibility, whatever its
    type), the Mach-O symbol table and the PE export directory;
  - the Windows import library must import exactly the DLL's exports, each by its own name from
    the bundle's DLL; other members may define only the import descriptor names, and the
    archive's symbol map must point each name at a member that defines it;
  - the prelinked object may define no non-local symbol besides the exports, the init function
    and the toolchain's names (common symbols, Mach-O tentative definitions included).

  These checks cover the tables listed; a file whose loader-visible data disagrees with them
  (an ELF `PT_DYNAMIC` that is not `.dynsym`, a Mach-O export trie) is #314.
- The checksum of a bundle is the content hash of its files (`vpm::native::bundle::checksum`,
  the hash `velt.lock.json` records for packages).

## Install

For the build target (`InstallOptions::target`):
- A **registry** package with `native` checksums in the lockfile: the bundle for the target is
  fetched into `<cache>/native/<name>-<version>/<triple>/`, after its checksum (and its
  `native.json`'s package, version and target) is verified in a staging directory; nothing
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
- A **path** package (or the root package) with `native` is built from source into
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

The bundled linker (lld with a link kit, [linking](../linking.md)) gets the same libraries
(`-rpath <dir>` instead of `-Wl,-rpath,<dir>`); a static musl executable
(`--target <arch>-unknown-linux-musl`) takes only `static/<p>.o`, so its native packages need a
`--release` build. The kits' system libraries cover what the runtime imports and, on Windows and
Linux, every export of those system libraries; a native library that needs another system
library links with `VELT_LINKER=system`.

The link stamp hashes every native file (path, size, modification time). A statically linked
(musl) `velt` cannot load libraries into its JIT host and says to use `velt dev --exe`.
`velt dev` watches the crate sources of libraries built from source and restarts (never swaps)
when they change.
