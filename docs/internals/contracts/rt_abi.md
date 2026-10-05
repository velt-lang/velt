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

## Native libraries of packages [additive]
Contract: [native_abi.md](native_abi.md). Called by `velt_main` before the user `main`, once per
package with a native library:

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_native_api` | `() -> const VeltRtApi*` | the function table handed to `velt_native_init_<pkg>` |
| `velt_rt_native_check` | `(int32_t rc, const VeltStr* package)` | `rc != 0`: prints that the package's native library failed to start, exits 1 |

## Strings [M1; representation: semantics stage 1; UTF-16 counts: #377 phase 1; boundaries: phase 2a; code-unit semantics: phase 2b]
Strings are immutable values (docs/internals/design/semantics.md): copying one never copies its bytes.
```c
typedef struct { uint64_t w0, w1, w2; } VeltStr;   // size 24, align 8 (vir::STR_AGG); little-endian
```
The bytes are **canonical WTF-8**: UTF-8 that may also hold a lone surrogate as a 3-byte sequence
(`ED A0..BF xx`), where a surrogate pair is always stored as its 4-byte code point (so byte
equality is code-unit equality). Every value also carries its **UTF-16 length** (code units,
[design/strings.md](../design/strings.md)); a string is ASCII exactly when its unit count equals
its byte count. `length` and every position count code units (#377 phase 2b).

Three forms, told apart by **byte 23** (the top byte of `w2`) and `w2`:

| Form | Test | Layout | Copy / drop |
|---|---|---|---|
| static / borrowed | byte 23 < 0x80 and `w2 == 0` | `{ptr, units << 32 \| len, 0}` | bitwise copy / nothing |
| inline, ASCII (≤ 23 bytes) | byte 23 ≥ 0x80, bit 0x40 clear | bytes 0..len hold the text, byte 23 = `0x80 \| len` (units = len) | bitwise copy / nothing |
| inline, non-ASCII (≤ 22 bytes) | byte 23 ≥ 0x80, bit 0x40 set | bytes 0..len hold the text, byte 22 = units, byte 23 = `0xC0 \| len`, plus `0x20` when it may hold lone surrogates | bitwise copy / nothing |
| heap | byte 23 < 0x80 and `w2 != 0` | `{ptr, units << 32 \| len, cap}`; `ptr` points into a refcounted buffer | count +1 / count −1, free at 0 |

- `w1` of the static and heap forms packs the unit count in its high 32 bits and the byte length
  in its low 32 bits. A string is shorter than 2 GiB (at most `i32::MAX` bytes): allocating a
  larger buffer is a fatal `string too long` (checked once, where the runtime computes a buffer's
  layout), and so is a borrowed view that long.
- The all-zero value is the empty static string. Literals are built by lowering as
  `{ &static_bytes, units << 32 | len, 0 }` (units counted from the literal's text). Sub-ranges of
  static strings may borrow them (same lifetime).
- Heap buffers come from the Rust global allocator (align 8); `ptr` is the address of the first
  byte, and the count is always the 8 bytes before it:
  - ASCII strings (units == len): `[count: u64 (atomic)][cap bytes]`;
  - non-ASCII strings: `[crumbs: pointer (atomic)][lone: u64][count: u64 (atomic)][cap bytes]`.
    `lone` is the number of lone surrogates in the text, or all ones when unknown (the buffer
    absorbed text from a static string, which has no room to record its count; whoever needs the
    number counts then, and records it: the field is accessed atomically, relaxed); `crumbs` is
    the breadcrumb table (below), null until a position in the string is first translated, and
    freed with the buffer.

  Which layout a buffer has follows from the value (units != len), so retaining needs nothing but
  `ptr`, and release, growth and free derive the header from the value. An inline string has
  no room for a lone count; its `0x20` bit is clear when it has none (set conservatively after
  a join, and for text from a static string, which can't tell without a scan). An ASCII buffer
  that receives its first non-ASCII byte moves the text behind a header at that append (the
  allocation is grown in place when the allocator can, and the text shifted),
  even when it is unique and has room. Only runtime functions allocate, share or free buffers.
  Counts are **atomic** (any string may cross threads: `spawn`, HTTP handlers, `shared`). The
  common case pays no atomic read-modify-write: dropping the only reference (count 1) frees
  after a plain load; an increment happens only when a string is copied while its source stays
  alive (the compiler moves instead when the source is dead).
- A buffer with count > 1 is never written. The builder (§12.1 of rt_abi_async.md) appends in
  place only to an inline string with room or a heap buffer with count 1 (of the right layout).
- Bytes enter a string through one runtime function (`VeltStr::push_wtf8`; every append, including
  `velt_rt_str_append` and the builder's pushes, ends there), which keeps the unit count, the lone
  count and the form in step in O(1) per append (geometric growth), and joins a high surrogate ending the string with a
  low one starting the appended text into the pair's 4-byte code point (only when both sides have
  lone surrogates: units are unchanged, bytes and lone count shrink by 2). Code outside the
  runtime's string module never writes `w1`/`w2`.
- Generated code reads the length (code units) inline:
  `byte23 ≥ 0x80 ? (byte23 & 0x40 ? byte22 : byte23 & 0x1f) : (int64_t)w1 >> 32` (the high
  half by an arithmetic shift, exact because strings are below 2 GiB; a zero-extending read
  lets LLVM vectorize index loops badly). `charCodeAt(i)` tests the form: an inline string with
  bit 0x40 of byte 23 clear, or a static/heap string whose halves of `w1` are equal, is ASCII,
  and the byte at `i` is loaded inline (bounds-checked against the length); any other string
  calls `velt_rt_str_char_code_at`. Strings are passed to the runtime by pointer for everything
  else. Test a form through byte 23: inline appends write single bytes
  into `w2`, and reading `w2` as a word right after would stall on store forwarding.
- Invariants, checked by a **debug** runtime on every append (each appended piece is canonical
  WTF-8 with the unit and lone counts it is given, and the seam is canonical: O(piece), never a
  recount of the whole string) and in full after every operation by the runtime's own tests: the
  bytes are canonical WTF-8; the stored unit count is the text's; a non-ASCII heap buffer's
  `lone`, unless unknown, is its number of lone surrogates; an inline string fits its form, and has no lone
  surrogates when its `0x20` bit is clear.
- **Well-formed text and output** (#377 phase 2a). A string is well-formed (valid UTF-16, so
  its bytes are UTF-8) exactly when it has no lone surrogates: decided by the value for ASCII,
  by the header's `lone` for a heap buffer, and by a scan of the bytes for a non-ASCII static
  string, an inline string with its `0x20` bit set or an unknown `lone`. Inside the runtime,
  `VeltStr::text()` gives a `&str` (zero-copy) only for well-formed text and the WTF-8 bytes
  (`Wtf8`) otherwise; operations that search, slice, split or build text work on those bytes,
  and pieces a result is assembled from join at their seams (`wtf8::push_joining`, or
  `push_wtf8`). Wherever text leaves the program or needs UTF-8 (stdout and stderr, files,
  sockets, WebSocket frames, HTTP bodies and headers, child-process arguments, environment and
  stdin, paths, process environment and working directory, database text and parameters, regex
  patterns, `TextEncoder`-style `u8[]` copies, panic and error messages, native packages) each
  lone surrogate becomes **one** U+FFFD (`EF BF BD`, 3 bytes like the surrogate, so the output
  is as long as the string's bytes); well-formed text is written as it is (a heap string's
  buffer becomes an HTTP body without a copy). `JSON.stringify` writes a lone surrogate as a
  lowercase `\udxxx` escape, and `console.log` of a string nested in a container (`inspect`
  quoting) as `\udxxx`; a top-level string prints U+FFFD. Text from outside (files, sockets,
  HTTP, stdin, child output, databases, `u8[]` decoding, native `str_new`, OS arguments,
  environment, paths and directory entries) is decoded as UTF-8, where a surrogate's encoding
  (`ED A0..BF xx`) is invalid: strict decoding refuses it, lossy decoding replaces it (WHATWG:
  one U+FFFD per maximal invalid subsequence, so a surrogate's three bytes give three U+FFFD),
  and nothing from outside ever makes a lone surrogate. On Windows the OS strings (arguments,
  environment, paths, directory entries) are UTF-16: they decode with one U+FFFD per unpaired
  surrogate.
- **Breadcrumbs** (`crumbs`, #377): for translating a code-unit index to a byte offset and back
  in a non-ASCII heap string of more than 64 units, the runtime builds on first use a table of
  `u32` byte offsets, one per 64th unit (top bit: that unit is the low half of the 4-byte
  sequence at the offset), publishes it in the header with a compare-and-swap and frees it with
  the buffer. A buffer that is appended to keeps its table (the prefix never changes), and a
  translation extends it when needed without trusting the count (a count-1 string may be read by
  two threads at once without a retain): entries are atomics written before the table's length
  covers them, and a table without room is replaced by a published copy twice its size, the old
  one kept alive until the buffer is freed. ASCII strings translate in O(1), other strings
  (short, inline, static) by a scan. Every code-unit position the runtime takes or returns goes
  through it. Each thread also remembers its last two translations of non-ASCII strings of more
  than 64 units, heap (with a table) or static (address, `w1`, form, position); a translation
  near one steps from it, so sequential index loops decode one character per step (in a static
  string, any forward step unless the end is closer). Freeing or growing a buffer that has a
  table first bumps a global epoch, which forgets every remembered position in a heap string (a
  new string at the same address is never taken for the old one). Positions in static strings
  never expire: a static non-ASCII string of more than 64 units points at a literal (the JSON
  reader copies such a key instead of borrowing it, rt_abi_async.md §12.3), and so must any
  other producer of borrowed views.
- `VELT_RC_STATS=1` with a **debug** runtime prints `rc stats: retain=… release=… alloc=… free=…`
  to stderr at exit (retain = increments, release = decrements of shared buffers, alloc/free =
  heap buffers). Release runtimes compile the counters out; to count optimized code, link a
  release-built program against the debug runtime (`VELT_RT_LIB=<target>/debug/velt_rt.lib`).
- `VELT_STDOUT_STATS=1` with a **debug** runtime prints `stdout stats: writes=<n>` to stderr at
  exit: how many times buffered stdout was written to the OS. Tests check buffering with it
  (piped output goes out in blocks: far fewer writes than lines) instead of timing a program.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_str_concat` | `(const VeltStr* a, const VeltStr* b, VeltStr* out)` | new string (an empty operand: the other one, shared) |
| `velt_rt_str_append` | `(VeltStr* s, const VeltStr* t)` | `s += t` on the owned string `*s`, whose old value is dead: in place when `*s` is inline with room or holds the only reference to its heap buffer (which grows geometrically); a static or shared `*s` is first copied into a buffer of its own. `t` may be `s` or lie in `*s`'s buffer. Emitted for `s += x`, `s = s + x` and `` s = `${s}${x}` `` on variables and fields (rt_abi_async.md §12.1) |
| `velt_rt_str_from_i64` | `(int64_t v, VeltStr* out)` | decimal (inline) |
| `velt_rt_str_from_u64` | `(uint64_t v, VeltStr* out)` | decimal (inline) |
| `velt_rt_str_from_f64` | `(double v, VeltStr* out)` | JS `Number.prototype.toString` formatting (inline) |
| `velt_rt_str_from_bool` | `(uint8_t v, VeltStr* out)` | `true`/`false` (static) |
| `velt_rt_str_clone` | `(const VeltStr* s, VeltStr* out)` | a copy: bitwise, plus count +1 for heap strings (never a deep copy) |
| `velt_rt_str_own` | `(const VeltStr* s, VeltStr* out)` | like `str_clone`, but a static-form string (which may borrow memory, e.g. a JSON key pointing into the parsed text) is copied into an inline or heap string |
| `velt_rt_str_drop` | `(VeltStr* s)` | count −1 for heap strings (frees at 0), then zeroes `*s` |
| `velt_rt_str_cmp` | `(const VeltStr* a, const VeltStr* b) -> int32_t` | -1 / 0 / 1 in UTF-16 code-unit order (`<`, `sort()`; #377 phase 2b): `memcmp` for two ASCII strings, else byte order corrected where it differs from code-unit order (design/strings.md "The ordering rule") |
| `velt_rt_str_char_code_at` | `(const VeltStr* s, int64_t i) -> int64_t` | the UTF-16 code unit at `i` (a supplementary character gives its high or low surrogate), -1 out of range; generated code calls it for non-ASCII strings only |
| `velt_rt_str_byte_length` | `(const VeltStr* s) -> uint64_t` | the UTF-8 length (`Buffer.byteLength(s)`): the stored byte length, O(1) |
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
