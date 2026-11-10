# std calls WASI through `declare c("c")` on wasm32-wasip1

Status: **implemented** (#2408, part of #2196; design review on #2461, 2026-11-06; implemented
with its first caller, `Math.random()`'s seed, in #2406). Owner: `velt_vir`
(`crates/velt_vir/src/lower/c_call.rs`, `crates/velt_vir/src/lower/system.rs`).

## Problem

On `wasm32-wasip1`, std can't call a WASI function. So anything std needs from WASI stays Rust
in `velt_rt_wasm`. The first case is the seed of `Math.random()`'s generator. #2406 moves the
generator into Velt, and its first version needed a new runtime function for the WebAssembly
seed, `velt_rt_prng_seed`, against #2196's rule of no new runtime functions.

Two routes exist, and both refuse on wasip1:

- `declare host("m") function` makes a WebAssembly import, but only for the JS host
  (`crates/velt_vir/src/lower/host.rs` `js_host`).
- `declare c("c") function` calls the C library. Lowering replaces every C call by an error
  when `system::target_os` is `None` (`crates/velt_vir/src/lower/c_call.rs:45`), and `target_os` is `None` for
  WebAssembly. The note says "C functions need a native target: WebAssembly can't link C
  libraries yet".

That second refusal is stale. Every wasm32-wasip1 program is already linked against wasi-libc's
`libc.a` (`crates/velt_link/src/wasm.rs`: `crt1-command.o`, the objects, `libvelt_rt_wasm.a`,
`libc.a`). That archive defines `getentropy`, `clock_gettime`, `read`, `write` and the rest of
WASI's POSIX layer, plus the raw `__wasi_*` wrappers. The accepted FFI design already says so:
library `c` is "WASI's libc on `wasm32-wasip1`" (c-ffi.md, "Libraries"), and WebAssembly has
"no libraries but `c` under WASI in the first steps".

## Design

**`declare c("c")` works on `wasm32-wasip1`, and nothing else is new.** There is no new syntax,
no WASI-specific declaration form, and no table naming WASI functions in the compiler
("pluggable, not hardcoded"). std declares the libc functions it uses, as it does for native
targets today (`std/ffi/read.velt` declares `strlen` and `memcpy`), and picks the file through an
import condition. `wasi` already exists in vpm's condition list for wasip1 (`crates/vpm/src/imports.rs`).

```ts
// std/lang/random/seed_wasi.velt: #random-seed's `wasi` variant (#2406)
declare c("c") function getentropy(buf: u8[], len: usize): i32;

/** 64 bits to seed a generator with, from WASI's random_get. */
export function seedWord(): u64 {
  const b: u8[] = Buffer.alloc(8);
  // SAFETY: `b` holds the 8 bytes getentropy writes.
  const rc = unsafe { getentropy(b, 8) };
  // random_get is a host call and may fail. Going on would seed every run with zeros.
  if (rc != 0) panic("WASI random_get failed: no seed for Math.random()");
  let w: u64 = 0;
  for (let i = 0; i < 8; i++) w = (w << 8) | (b[i] as u64);
  return w;
}
```

```json
"#random-seed": {
  "wasi": "./lang/random/seed_wasi.velt",
  "js": "./lang/random/seed_js.velt",
  "default": "./lang/random/seed_native.velt"
}
```

### Compiler changes

1. **Lowering.** In `c_call` (and `c_const`), the target gate stops being
   `target_os(target).is_none()`. A C call is available when the target is native, or when the
   target is `wasm32-wasip1` and the library is `c`.
2. **ABI on wasm32.** Pointers are 32 bits there, and so are C's `size_t` and `ssize_t`, while
   Velt's `usize` and `isize` are 64 bits on every target. A `usize`/`isize` parameter therefore
   crosses as a 32-bit integer on wasm32: the argument is truncated at the call, and a result is
   widened back (`c_vir`, `narrow`). Every other C type crosses unchanged. `errno` isn't
   captured on WebAssembly: `lastOsError()` has no wasm overload, so `capture_errno` does
   nothing there, as before. A declaration that asks for `errno` still compiles.
3. **Diagnostics.**
   The error stays "C function `x` is not available on `<target>`" (at the user's call when std
   is the caller, as now). Its note says why:
   - another library on wasip1: "`sqlite3` has no library on wasm32-wasip1: only the C library
     `c` (wasi-libc) is linked there";
   - a package's call of `c` on wasip1: "C functions in packages need a native target on
     wasm32-wasip1 until their headers can be checked";
   - `wasm32-unknown-unknown` (browser, workers): "C functions need a native target or
     wasm32-wasip1: the JS host has no C library, and std reaches the host through
     `declare host`".
4. **Header check.** Unchanged. std declarations are not checked against headers on any target
   (`crates/veltc/src/native/headers/mod.rs:91`). The gate opens by target and library, not by
   std, so a package's `declare c("c")` on wasip1 becomes reachable too. Its declarations can't be
   checked there while `zig_target` has no WASI entry, so a package's C call on wasip1 is refused
   ("C functions in packages need a native target on `wasm32-wasip1` until their headers can be
   checked") until zig's `wasm32-wasi` headers are wired in. That is a follow-up, not needed for
   std.
5. **`declare system`.** Not extended to wasm, and gone since #2187 step 2: std's file
   functions are `declare c("c")` on macOS and Linux, and fail with ENOSYS on wasip1 (the
   `#fd-system` variant `unsupported.velt`) until a wasip1 variant reads wasi-libc's `struct
   stat`.

No linker change: `libc.a` is already on the link line, and library `c` adds nothing to
`c_libs`.

### The browser flavor

`wasm32-unknown-unknown` has no libc, so its seed goes through a `declare host` function in a
std glue module, like `velt:fetch`:

```ts
// std/lang/random/seed_js.velt
declare host("velt:random") function fill(buf: u8[]): void;   // crypto.getRandomValues
```

A `u8[]` already crosses as `(ptr, len)` into linear memory (`crates/velt_vir/src/lower/host.rs`). The glue
(`crates/velt_rt_wasm/js/velt_web.mjs`, and every host that spreads std's modules: `crates/velt_rt_wasm/js/velt_serve.mjs`,
`host-cloudflare`, the playground) implements it as
`crypto.getRandomValues(new Uint8Array(memory.buffer, ptr, len))`. `crates/veltc/src/driver/host_modules.rs` already
checks the glue against std's declarations. This is JS host glue, not runtime Rust, and it lands
with #2406.

## Cost

- **Run time:** one `getentropy` call per thread, at the first random number (one `random_get`
  import call). Nothing changes on native targets.
- **Build time:** none. One more condition variant, and the module is parsed only when
  `#random-seed` resolves to it.
- **Runtime size:** `crates/velt_rt_wasm/src/prng.rs` goes (in #2406). `getentropy` was already linked,
  because Rust's `RandomState` used it.

## Tests

- **Unit (`crates/velt_vir/src/tests/c_wasi.rs`, real std):**
  - std's `readCString` (libc's `strlen` and `memcpy`) lowers on wasm32-wasip1 with `size_t` as
    `u32`, and on aarch64-apple-darwin as `u64`;
  - on wasm32-unknown-unknown it is refused with the JS-host note;
  - a program's own `declare c("c")` and `declare c("z")` are refused on wasip1 with their notes.
- **Integration (`crates/veltc/tests/c_functions.rs`):** a package's C call is refused on both
  WebAssembly targets with the right note.
- **End to end:**
  - `Math.random()`'s seed is wasi-libc's `getentropy`.
  - `std/random` runs in the WASI goldens.
  - `math_random_seeds_each_run_on_webassembly` (`crates/veltc/tests/wasm_goldens.rs`) runs a
    program twice under wasmtime and twice in the browser glue under node, and expects different
    numbers.
  - `a_failed_wasi_seed_panics` runs it under node:wasi with a `random_get` that returns EIO, and
    expects a panic with exit 101.
  - The `std/random_runs` golden starts child processes, so it stays native-only.
- **Deliberate breaks:**
  - leaving the gate as it was fails the std unit test;
  - dropping the 32-bit `size_t` fails it too;
  - opening the gate to every file fails the package test;
  - a constant WASI seed, a `fill` that does nothing, and a `getentropy` result left unchecked
    each fail one of the two WebAssembly tests.
  - Opening it to every library fails nothing, as expected: std may only name `c`
    (`crates/veltc/src/native/c_decls.rs` stops on any other library), and a non-std call is
    refused before the library matters.

## Follow-ups

- **`Out<usize>` and `OutPtr` on wasm32:** these cross by address with no narrowing, so C would
  write 4 bytes into an 8-byte field. Nothing reachable declares one today: std's only C calls
  on wasip1 are `strlen`, `memcpy` and `getentropy`, and packages are refused. Narrow them (a
  32-bit temporary, widened after the call) before the first such declaration.
- Other `velt_rt_wasm` code that only wraps WASI (clocks, environment, files) can move to std the
  same way, each in its own #2196 step.
