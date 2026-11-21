# C calls on wasm32-wasip1

Status: implemented. Issue: [#2408](https://github.com/velt-lang/velt/issues/2408).

## Summary

On `wasm32-wasip1`, std calls WASI through the C library: a `declare c("c") function` in std
binds a function of wasi-libc, which every WASI program is linked against. There is no new
syntax, no WASI-specific declaration form and no table of WASI functions in the compiler. std
declares the libc functions it uses, as it does on native targets, and picks the file for each
target through an import condition. What std needs from WASI is therefore Velt code in std, not
Rust in `velt_rt_wasm`.

## Motivation

`Math.random()`'s generator is Velt code (`std/lang/random/wyrand.velt`), and it needs 64 random
bits for its seed. On native targets those come from the C library; under WASI they come from the
host's `random_get`. wasi-libc wraps `random_get` as `getentropy`, and every `wasm32-wasip1`
program is linked against wasi-libc's `libc.a` already (`crates/velt_link/src/wasm.rs` links
`crt1-command.o`, the program's objects, `libvelt_rt_wasm.a` and `libc.a`). That archive
defines `getentropy`, `clock_gettime`, `read`, `write` and the rest of WASI's POSIX layer, plus
the raw `__wasi_*` wrappers. Library `c` on `wasm32-wasip1` is wasi-libc
([c-ffi.md](c-ffi.md)), so std can reach WASI without a runtime function written for it.

## Design

std declares the C function and selects the module through an import condition. `wasi` is one
of vpm's conditions for `wasm32-wasip1` (`crates/vpm/src/imports.rs`):

```ts
// std/lang/random/os_wasi.velt
declare c("c") function getentropy(buf: Ptr<u8>, len: usize): i32;

/** Fills the `n` bytes at `base` with the host's random bytes. Stops the program if `random_get`
 * fails. */
export unsafe function fillRandomAt(base: usize, n: usize): void {
  let done: usize = 0;
  while (done < n) {
    const k = n - done < 256 ? n - done : 256; // getentropy's most per call
    // SAFETY: bytes `done..done + k` of the caller's memory, at most 256.
    if (unsafe { getentropy(Ptr.at<u8>(base + done), k) } != 0) {
      panic("WASI random_get failed: no random numbers");
    }
    done += k;
  }
}
```

```ts
// std/imports.velt
"#os-random": {
  darwin: "./lang/random/os_darwin.velt",
  linux: "./lang/random/os_linux.velt",
  win32: "./lang/random/os_win32.velt",
  wasi: "./lang/random/os_wasi.velt",
  js: "./lang/random/os_js.velt",
},
```

std/lang/random/os.velt builds `osRandomBytes` and `osRandomU64` (the seed of `Math.random()`,
velt/crypto, UUIDs) on the variant's `fillRandomAt`.

A draw panics rather than throws when `random_get` fails: `Math.random()` throws nothing, and
its callers must not have to handle an exception because of it.

### Who may call C on WebAssembly

| Target | std, library `c` | std, other library | package |
|---|---|---|---|
| native | yes | yes | yes |
| `wasm32-wasip1` | yes (wasi-libc) | never declared | refused |
| `wasm32-unknown-unknown` | refused | never declared | refused |

std only ever names library `c` (`crates/veltc/src/native/c_decls.rs` reports an ICE for any
other). A package's `declare c("c")` on `wasm32-wasip1` is refused because its declarations
cannot be checked against WASI's headers: the header check needs a zig target, and
`velt_toolchain::zig::target` has no WASI entry.

### The browser flavor

`wasm32-unknown-unknown` (browsers, workers) has no C library, so its random numbers go through a
`declare host` function in a std glue module:

```ts
// std/lang/random/os_js.velt
declare host("velt:random") function fill(buf: u8[]): void;
```

A `u8[]` crosses as `(ptr, len)` into linear memory (`crates/velt_vir/src/lower/host.rs`). The
JS glue implements it as `randomHost` in `crates/velt_rt_wasm/js/velt_web.mjs` with
`crypto.getRandomValues` over the program's memory; hosts that load std's modules through
`velt_web.mjs` (`velt_serve.mjs`, `packages/host-cloudflare`) get it from there.
`crates/veltc/src/driver/host_modules.rs` checks the glue's functions against std's
declarations.

## Compilation

- **Gate.** `FnLower::c_callable` in `crates/velt_vir/src/lower/c_call.rs` accepts a C call
  when the target is native, or when the target is `wasm32-wasip1` (or its older name
  `wasm32-wasi`), the library is `c` and the declaration is in std. Any other call is replaced
  by an error once lowering is done (`unavailable_diagnostics` in
  `crates/velt_vir/src/lower/extern_args.rs`). C constants (`declare c const`) need C headers
  and are unavailable on WebAssembly.
- **ABI on wasm32.** Pointers, C's `size_t` and C's `ssize_t` are 32 bits there, while Velt's
  `usize` and `isize` are 64 bits on every target. A `usize`/`isize` parameter crosses as a
  32-bit integer: the argument is truncated at the call and a result is widened back (`c_vir`
  and `narrow` in `c_call.rs`). Every other C type crosses unchanged.
- **`errno`.** WebAssembly has no `errno` source in `c_abi::errno_source`, so `capture_errno`
  emits nothing there. A declaration that asks for `errno` still compiles.
- **Linking.** No change: `libc.a` is on every `wasm32-wasip1` link line, and library `c` adds
  nothing to the libraries a program links.
- **Header check.** std's declarations in the files `crates/veltc/src/native/headers/std_check.rs`
  lists are checked against the C library's headers by `velt native headers --std` on native
  targets; builds take them as checked. There is no WASI header check.
- **Tests.** `crates/velt_vir/src/tests/c_wasi.rs` lowers std's `readCString` (libc's `strlen`
  and `memcpy`, `std/ffi/read.velt`) on `wasm32-wasip1` with `size_t` as `u32` and on
  `aarch64-apple-darwin` as `u64`, and checks the refusals and their notes.
  `crates/veltc/tests/c_functions.rs` checks that a package's C call is refused on both
  WebAssembly targets. `crates/veltc/tests/wasm_goldens.rs` runs the `std/random`, `std/crypto`,
  `std/crypto_random_int` and `std/uuid` goldens under WASI;
  `crypto_random_bytes_fill_large_draws_on_webassembly` draws up to 200,000 bytes on both targets,
  past the JS host's 65,536 bytes a call; `math_random_seeds_each_run_on_webassembly` runs a program twice under wasmtime and twice
  in the browser glue under Node and expects different numbers; `a_failed_wasi_seed_panics` runs
  it under `node:wasi` with a `random_get` that returns `EIO` and expects a panic (exit 101).

## Runtime cost

- **Run time:** `Math.random()` makes one `getentropy` call (one `random_get` import call) per
  thread, at its first number. velt/crypto's `randomBytes` and `randomInt` and the UUIDs make
  one per draw (a 256-byte chunk each), as they make one OS call per draw on native targets.
- **Build time:** none measurable: one more condition variant, parsed only when `#os-random`
  resolves to it.
- **Runtime size:** the runtime has no random function on any target.

## Diagnostics

A refused call reports "C function `x` is not available on `<target>`" at the declaration. When
std declares the function, the error is at the user's call of the std function instead,
"`<std function>` is not available on `<target>`", with the note "it calls the C function `x`".
Either way a second note says why:

| Case | Note |
|---|---|
| another library on `wasm32-wasip1` | "`sqlite3` has no library on wasm32-wasip1: only the C library `c` (wasi-libc) is linked there" |
| a package's call of `c` on `wasm32-wasip1` | "C functions in packages need a native target on wasm32-wasip1 until their headers can be checked" |
| `wasm32-unknown-unknown` | "C functions need a native target or wasm32-wasip1: the JS host has no C library, and std reaches the host through `declare host`" |

## Alternatives considered

**A WASI-specific declaration form or a compiler table of WASI functions.** It would name a
platform in the compiler, where host and platform details belong in std and packages, and it
would duplicate what wasi-libc already exposes through the C ABI.

**`declare host` on WASI.** `declare host` makes a WebAssembly import, but it is only for the
JS host (`js_host` in `crates/velt_vir/src/lower/host.rs`). Calling wasi-libc keeps std's WASI
code on the same path as its native C calls.

**A runtime function for the seed.** It would put new Rust in `velt_rt_wasm` for one call that
wasi-libc already provides.

## Limitations

- Packages cannot call C on WebAssembly.
- std's file functions (`#fd-system`) have variants for macOS and Linux only; on `wasm32-wasip1`
  the `unsupported.velt` variant is used.
- `Out<T>` and `OutPtr` arguments cross by address with no narrowing, so on wasm32 a C function
  would write 4 bytes into an 8-byte `usize` field. A std declaration reachable on WebAssembly
  must not pass a `usize` or `isize` field that way.
