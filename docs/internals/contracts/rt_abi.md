# Runtime C ABI (velt_rt) — CONTRACT

Implemented by `crates/velt_rt` (Rust, `extern "C"`, `#[no_mangle]`). Called by code emitted from VIR
(`vir::ExternFn` entries created by lowering). All pointers are 64-bit. `bool` is `u8` (0/1).
Integers narrower than 64 bits are sign/zero-extended by the caller to the parameter type listed.

## Process entry [M1]
- The runtime defines the C `main(argc, argv)` (must be `#[cfg(not(test))]` so rlib tests still link).
  It initializes the runtime, stores args, calls `velt_main`, flushes stdout, returns its result.
- Generated code exports `int32_t velt_main(void)` (created by lowering; returns 0 for `void` mains).
- `int velt_rt_start(int argc, char** argv, int32_t (*entry)(void))` does what `main` does with
  `entry` as the program's main. The shared runtime (debug builds) has no `main`: the compiler
  emits a small entry object whose `main` calls `velt_rt_start(argc, argv, &velt_main)`.

## Strings [M1; representation: semantics stage 1]
Strings are immutable values (docs/internals/design/semantics.md): copying one never copies its bytes.
```c
typedef struct { uint64_t w0, w1, w2; } VeltStr;   // size 24, align 8 (vir::STR_AGG); little-endian
```
Three forms, told apart by **byte 23** (the top byte of `w2`) and `w2`:

| Form | Test | Layout | Copy / drop |
|---|---|---|---|
| static / borrowed | byte 23 < 0x80 and `w2 == 0` | `{ptr, len, 0}` | bitwise copy / nothing |
| inline (≤ 23 bytes) | byte 23 ≥ 0x80 | bytes 0..len hold the text, byte 23 = `0x80 \| len` | bitwise copy / nothing |
| heap | byte 23 < 0x80 and `w2 != 0` | `{ptr, len, cap}`; `ptr` points into a refcounted buffer | count +1 / count −1, free at 0 |

- The all-zero value is the empty static string. Literals are built by lowering as
  `{ &static_bytes, len, 0 }`. Sub-ranges of static strings may borrow them (same lifetime).
- Heap buffers: `[count: u64 (atomic)][cap bytes]` from the Rust global allocator (align 8);
  `ptr` is the address after the count. Only runtime functions allocate, share or free them.
  Counts are **atomic** (any string may cross threads: `spawn`, HTTP handlers, `shared`). The
  common case pays no atomic read-modify-write: dropping the only reference (count 1) frees
  after a plain load; an increment happens only when a string is copied while its source stays
  alive (the compiler moves instead when the source is dead).
- A buffer with count > 1 is never written. The builder (§12.1 of rt_abi_async.md) appends in
  place only to an inline string with room or a heap buffer with count 1.
- Generated code reads the length inline (branch-free: `byte23 ≥ 0x80 ? byte23 & 0x7f : w1`)
  and passes strings to the runtime by pointer for everything else. Test a form through byte 23:
  inline appends write single bytes into `w2`, and reading `w2` as a word right after would stall
  on store forwarding.
- `VELT_RC_STATS=1` with a **debug** runtime prints `rc stats: retain=… release=… alloc=… free=…`
  to stderr at exit (retain = increments, release = decrements of shared buffers, alloc/free =
  heap buffers). Release runtimes compile the counters out; to count optimized code, link a
  release-built program against the debug runtime (`VELT_RT_LIB=<target>/debug/velt_rt.lib`).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_str_concat` | `(const VeltStr* a, const VeltStr* b, VeltStr* out)` | new string (an empty operand: the other one, shared) |
| `velt_rt_str_from_i64` | `(int64_t v, VeltStr* out)` | decimal (inline) |
| `velt_rt_str_from_u64` | `(uint64_t v, VeltStr* out)` | decimal (inline) |
| `velt_rt_str_from_f64` | `(double v, VeltStr* out)` | JS `Number.prototype.toString` formatting (inline) |
| `velt_rt_str_from_bool` | `(uint8_t v, VeltStr* out)` | `true`/`false` (static) |
| `velt_rt_str_clone` | `(const VeltStr* s, VeltStr* out)` | a copy: bitwise, plus count +1 for heap strings (never a deep copy) |
| `velt_rt_str_drop` | `(VeltStr* s)` | count −1 for heap strings (frees at 0), then zeroes `*s` |
| `velt_rt_str_cmp` | `(const VeltStr* a, const VeltStr* b) -> int32_t` | bytewise: -1 / 0 / 1 |
| `velt_rt_str_hash` | `(const VeltStr* s) -> uint64_t` | hash of the bytes (`Map`/`Set` keys; fixed seed) |

## Counted objects [semantics stage 2]
Compiled code only (no runtime functions): values the program shares
([semantics-stage2.md](../design/semantics-stage2.md)) live in heap blocks `[count: u64][value]`
allocated with `velt_rt_alloc(8 + size, 8)`; the value pointer is the block address + 8, so it is
also what a borrow of the value passes (`T*`). Counts are plain (non-atomic) — counted objects never
cross threads — and are updated inline. The runtime only ever sees values in their unboxed layout:
an extern parameter or result whose type holds boxed values crosses as an unboxed view (arguments)
or is boxed after the call (results). Closure environments on the heap are counted blocks too; the
drop function in their header releases one reference.

## Output [M1]
`stream`: 1 = stdout (buffered, flushed at exit / before any stderr write / on `velt_rt_flush`), 2 = stderr.

| Symbol | Signature |
|---|---|
| `velt_rt_write_str` | `(uint32_t stream, const VeltStr* s)` |
| `velt_rt_write_i64` | `(uint32_t stream, int64_t v)` |
| `velt_rt_write_u64` | `(uint32_t stream, uint64_t v)` |
| `velt_rt_write_f64` | `(uint32_t stream, double v)` — JS formatting (`1`, `1.5`, `1e+21`, `NaN`) |
| `velt_rt_write_bool` | `(uint32_t stream, uint8_t v)` |
| `velt_rt_write_byte` | `(uint32_t stream, uint8_t b)` — used for `' '` and `'\n'` |
| `velt_rt_flush` | `(void)` |

`console.log(a, b)` lowers to: `write_<a>(1, a); write_byte(1, ' '); write_<b>(1, b); write_byte(1, '\n')`.
`f32` values are converted to `f64` before `write_f64`; narrower ints extended to 64-bit.

## Memory [M1]
| Symbol | Signature |
|---|---|
| `velt_rt_alloc` | `(uint64_t size, uint64_t align) -> void*` — aborts on OOM, never null for size > 0 |
| `velt_rt_realloc` | `(void* p, uint64_t old_size, uint64_t align, uint64_t new_size) -> void*` |
| `velt_rt_free` | `(void* p, uint64_t size, uint64_t align)` |

`VELT_RT_DEBUG_ALLOC=1` with a **debug** runtime (programs linked against `<target>/debug/velt_rt`,
e.g. every golden run) checks every allocation of the process (generated code and runtime alike):
blocks carry a header and canaries and start filled with `0xCD`; a free checks for double
frees, foreign pointers, a size different from the allocation's and overwritten canaries
(overflow/underflow), fills the block with `0xDD` and keeps it in a quarantine (64 MiB) so a use
after free reads the poison instead of a reused block; a write after free is detected when the
block leaves the quarantine. The first violation prints `velt debug-alloc: <what> (block …, size
…)` to stderr and aborts. Release runtimes don't contain the check.

## Control [M1]
| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_panic` | `(const VeltStr* msg) -> noreturn` | flush stdout, print `panic: <msg>\n` to stderr, exit 101 |
| `velt_rt_exit` | `(int32_t code) -> noreturn` | flush stdout, exit |
| `velt_rt_set_throw_loc` | `(const VeltStr* loc)` | per-thread slot: where the error being thrown comes from (static ` at <path>:<line>:<col>` string, or null) |
| `velt_rt_throw_loc` | `(void) -> const VeltStr*` | the slot's value (null if never set); appended to `Uncaught …` reports |
| `velt_rt_pow_f64` | `(double a, double b) -> double` | |
| `velt_rt_pow_i64` | `(int64_t a, int64_t b) -> int64_t` | wrapping; negative exponent → 0 (1 if a == 1) |

Division by zero: lowering emits a check that calls `velt_rt_panic` with message `division by zero`.
With source locations (`lower_with`), compiler-emitted panic messages end in
` at <path>:<line>:<col>` (e.g. `division by zero at examples/foo.vlt:12:7`).

## Async, I/O, HTTP, sync helpers [M3/M4 — frozen]
See `docs/internals/contracts/rt_abi_async.md` (state-machine poll/drop protocol, `VeltFut`, spawn/join,
`Promise.all`, sleep/yield, fs/net/http/process, `VeltErr`/`IoResult`, atomics, refcounts, mutex,
hashing, math).

## Byte arrays, BigInt, number formatting, binary stdio [perf-std, additive]
Declared in std (Velt signatures; `u8[]`/`string` are passed as in rt_abi_async.md, handles are
`u64`), checked against the runtime by `crates/velt_rt/tests/std_externs.rs`:
- `u8[]` (`std/prelude/bytes.vlt`): `velt_rt_bytes_zeroed(n: u64): u8[]`,
  `velt_rt_bytes_index_of(b: u8[], byte: u8, from: i64): i64`,
  `velt_rt_bytes_last_index_of(b: u8[], byte: u8, from: i64): i64` (−1 if absent),
  `velt_rt_bytes_set(dst: u8[], src: u8[], offset: u64): bool` (false: out of range),
  `velt_rt_bytes_copy_within(b: u8[], target: i64, start: i64, end: i64)`,
  `velt_rt_bytes_fill(b: u8[], byte: u8, start: i64, end: i64)`.
- BigInt (`std/bigint.vlt`, handle `u64`, freed by `velt_rt_bigint_free`): `from_i64`, `from_f64`,
  `parse(s, radix)`, `clone`, `assign(dst, a)`, `neg(dst, a)`, `op(dst, a, b, op: u32): bool` and
  `op_i64(dst, a, k: i64, op: u32): bool` (false: division by zero or a negative or huge shift; `dst` unchanged), `cmp`/`cmp_i64` (−1/0/1),
  `to_i64`, `to_f64`, `to_string(a, radix)`. `op` codes are defined in `std/bigint.vlt`.
- Numbers: `velt_rt_f64_to_fixed(x: f64, digits: i64): string` (JS `toFixed` rounding),
  `velt_rt_math_umulh(a: u64, b: u64): u64` (high 64 bits of the product).
- Binary stdio: `velt_rt_stdout_write_bytes(bytes: u8[])` (synchronous, ordered with
  `console.log`), `async velt_rt_stdin_read_all_bytes(): IoResult<u8[]>` and
  `velt_rt_stdin_read_all_bytes_sync(): IoResult<u8[]>` (hand over the runtime's buffer).
