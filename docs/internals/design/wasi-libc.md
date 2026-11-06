# std calls WASI through `declare c("c")` on wasm32-wasip1

Status: **proposed** (#2408, part of #2196). Owner: `velt_vir` (`lower/c_call.rs`,
`lower/system.rs`). Unblocks #2406.

## Problem

On `wasm32-wasip1`, std can't call a WASI function. So anything std needs from WASI stays Rust
in `velt_rt_wasm`. The first case is the seed of `Math.random()`'s generator. #2406 moves the
generator into Velt, but it still needs a new runtime function for the WebAssembly seed,
`velt_rt_prng_seed`, against #2196's rule of no new runtime functions.

Two routes exist, and both refuse on wasip1:

- `declare host("m") function` makes a WebAssembly import, but only for the JS host
  (`lower/host.rs` `js_host`).
- `declare c("c") function` calls the C library. Lowering replaces every C call by an error
  when `system::target_os` is `None` (`lower/c_call.rs:45`), and `target_os` is `None` for
  WebAssembly. The note says "C functions need a native target: WebAssembly can't link C
  libraries yet".

That second refusal is stale. Every wasm32-wasip1 program is already linked against wasi-libc's
`libc.a` (`velt_link/src/wasm.rs`: `crt1-command.o`, the objects, `libvelt_rt_wasm.a`,
`libc.a`). That archive defines `getentropy`, `clock_gettime`, `read`, `write` and the rest of
WASI's POSIX layer, plus the raw `__wasi_*` wrappers. The accepted FFI design already says so:
library `c` is "WASI's libc on `wasm32-wasip1`" (c-ffi.md, "Libraries"), and WebAssembly has
"no libraries but `c` under WASI in the first steps".

## Design

**`declare c("c")` works on `wasm32-wasip1`, and nothing else is new.** There is no new syntax,
no WASI-specific declaration form, and no table naming WASI functions in the compiler
("pluggable, not hardcoded"). std declares the libc functions it uses, as it does for native
targets today (`std/ffi/read.velt` declares `strlen` and `memcpy`), and picks the file through an
import condition. `wasi` already exists in vpm's condition list for wasip1 (`vpm/src/imports.rs`).

```ts planned
// std/lang/random/seed_wasi.velt: #random-seed's `wasi` variant (#2406)
declare c("c") function getentropy(buf: u8[], len: usize): i32;

/** 64 bits to seed a generator with, from WASI's random_get. */
export function seedWord(): u64 {
  const b: u8[] = Buffer.alloc(8);
  // SAFETY: `b` holds the 8 bytes getentropy writes; it never fails for len <= 256 under WASI.
  unsafe {
    getentropy(b, 8);
  }
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
2. **ABI on wasm32.** Lowering C types needs pointer, `usize` and `isize` to be 32 bits wide on
   wasm32 (`long` is 32 bits there too). The C-call path already gets these sizes from the
   target. The implementation checks that every `CType` used by `std` declarations lowers
   correctly for wasm32 and that `capture_errno` works with wasi-libc's `errno`, which is a
   plain global (no TLS model under `wasm32-wasip1`).
3. **Diagnostics.**
   - Another library on wasip1 gets the error the FFI design already specifies: "`sqlite3_open_v2`
     has no library on `wasm32-wasip1`". It is reported at the user's call when std is the caller,
     as now.
   - `wasm32-unknown-unknown` (browser, workers) keeps the refusal, with a new note: "the JS host
     has no C library; std reaches the host through `declare host`".
4. **Header check.** Unchanged. std declarations are not checked against headers on any target
   (`native/headers/mod.rs:91`). Package declarations on wasip1 stay unchecked while `zig_target`
   has no WASI entry, which matches today's behavior for packages (they can't use C on wasip1
   anyway, since only `c` is allowed). Adding zig's `wasm32-wasi` headers is a follow-up, not
   needed for std.
5. **`declare system`.** Not extended to wasm. The FFI design folds it into `declare c`, so a
   wasip1 table for it would be work thrown away.

No linker change: `libc.a` is already on the link line, and library `c` adds nothing to
`c_libs`.

### The browser flavor

`wasm32-unknown-unknown` has no libc, so its seed goes through a `declare host` function in a
std glue module, like `velt:fetch`:

```ts planned
// std/lang/random/seed_js.velt
declare host("velt:random") function fill(buf: u8[]): void;   // crypto.getRandomValues
```

A `u8[]` already crosses as `(ptr, len)` into linear memory (`lower/host.rs`). The glue
(`velt_rt_wasm/js/velt_web.mjs`, and every host that spreads std's modules: `velt_serve.mjs`,
`host-cloudflare`, the playground) implements it as
`crypto.getRandomValues(new Uint8Array(memory.buffer, ptr, len))`. `host_modules.rs` already
checks the glue against std's declarations. This is JS host glue, not runtime Rust, and it lands
with #2406.

## Cost

- **Run time:** one `getentropy` call per thread, at the first random number (one `random_get`
  import call). Nothing changes on native targets.
- **Build time:** none. One more condition variant, and the module is parsed only when
  `#random-seed` resolves to it.
- **Runtime size:** `velt_rt_wasm/src/prng.rs` goes (in #2406). `getentropy` was already linked,
  because Rust's `RandomState` used it.

## Tests

- **Unit (`velt_vir`):** a std `declare c("c")` call lowers to a C call on wasm32-wasip1. A
  `declare c("z")` call there gives "has no library on `wasm32-wasip1`". On wasm32-unknown-unknown,
  the call gives the JS-host note. Replaces the `pread`/`wasm32-wasip1` expectation in
  `veltc/src/native/tests.rs` where it covers `declare c`.
- **wasm golden (`wasm_goldens`, wasip1 under wasmtime):** a std function calling libc's
  `getentropy` and `clock_gettime` runs and prints plausible results. In #2406, the
  `std/random_runs` golden joins `WASI_STD`.
- **Deliberate breaks:** leaving the gate as it was fails the unit test and the golden. Allowing
  any library on wasip1 fails the "no library" test.

## Afterwards

- #2406: `seed_wasi.velt` and `seed_js.velt` replace `seed_wasm.velt`, and `velt_rt_prng_seed`
  and `velt_rt_wasm/src/prng.rs` are deleted.
- Other `velt_rt_wasm` code that only wraps WASI (clocks, environment, files) can move to std the
  same way, each in its own #2196 step.
