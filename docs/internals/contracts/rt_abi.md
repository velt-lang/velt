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
| heap | byte 23 < 0x80 and `w2 != 0` | `{ptr, units << 32 \| len, cap}`; `ptr` points into a refcounted buffer; a **slice** has bit 62 of `w2` set and other bits there (below) | count +1 / count −1, free at 0 |

- `w1` of the static and heap forms packs the unit count in its high 32 bits and the byte length
  in its low 32 bits. A string is shorter than 2 GiB (at most `i32::MAX` bytes): allocating a
  larger buffer is a fatal `string too long` (checked once, where the runtime computes a buffer's
  layout), and so is a borrowed view that long.
- The all-zero value is the empty static string. Literals are built by lowering as
  `{ &static_bytes, units << 32 | len, 0 }` (units counted from the literal's text). Sub-ranges of
  static strings may borrow them (same lifetime).
- Heap buffers come from the Rust global allocator (align 8); `ptr` is the address of the first
  byte (of the buffer's own string; a slice's `ptr` points further in, see below), and the count
  is always the 8 bytes before the buffer's first byte:
  - ASCII strings (units == len): `[count: u64 (atomic)][cap bytes]`;
  - non-ASCII strings: `[crumbs: pointer (atomic)][lone: u64][count: u64 (atomic)][cap bytes]`.
    `lone` is the number of lone surrogates in the text, or all ones when unknown (the buffer
    absorbed text from a static string, which has no room to record its count; whoever needs the
    number counts then, and records it: the field is accessed atomically, relaxed); `crumbs` is
    the breadcrumb table (below), null until a position in the string is first translated, and
    freed with the buffer (or the value 1 when there is no table but a thread remembers a
    position in a slice of the buffer, which makes freeing it bump the epoch, as a table does).

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
- A **slice** (#402) is a heap string whose bytes lie inside another heap string's buffer, which
  it shares through the buffer's count: `{ptr, units << 32 | len, w2}`, with `ptr` at the
  slice's first byte and `w1` its own counts. `w2` tells the two heap forms apart:

  | bits of `w2` | plain heap string | slice |
  |---|---|---|
  | 63 | 0 | 0 |
  | 62 | 0 | 1 |
  | 61 | 0 | 1 when the buffer has a header (its text is not ASCII, whatever the slice's is) |
  | 31..60 | 0 | the byte offset of `ptr` from the buffer's first byte |
  | 30 | capacity (bits 0..30) | 0 |
  | 0..29 | capacity | the buffer's capacity |

  For every heap string the buffer's first byte is `ptr - ((w2 >> 31) & (2^30 - 1))` (0 for a
  plain one, whose capacity is below 2^31), and its count is the 8 bytes before that.
- **Which pieces share.** Exactly these runtime functions return a slice of a heap string:
  `velt_rt_str_slice` (`slice`, `substring`, `charAt`, `at`, `s[i]`),
  `velt_rt_str_trim`, `velt_rt_str_trim_start`, `velt_rt_str_trim_end`, and
  `velt_rt_str_split` when its separator is not empty and cannot match half of a surrogate pair.
  Every other function copies the pieces it returns: `split("")`, a split by a lone surrogate,
  `velt_rt_str_code_points`, and the regex functions (the pieces of `match`, `exec` and `split`
  are new strings, `VeltStr::from_vec`). Those four return a slice
  when the piece is too long to be inline, at least a quarter of the buffer's capacity, and the
  buffer is below 1 GiB (2^30 bytes); otherwise it copies (inline when short).
  The whole string is shared as it is (count +1, same value), a piece of a static string
  borrows it, and a piece that ends between the halves of a surrogate pair is a copy (the half
  is re-encoded). So live slices pin at most four times their own size (a short piece of a huge
  string never keeps it alive), and a parser that consumes its input from the front
  (`rest = rest.slice(n)`) copies at most a third of its buffer's capacity in all: a third of
  the input when the capacity is its length, up to about two thirds when appends grew the
  buffer (capacity up to twice the length). Linear either way.
- **Counts and threads.** A buffer's count counts every string that references it, slices
  included: a slice retains the buffer when made and releases it when dropped, and the last
  reference, plain or slice, frees it with the buffer's layout (bit 61 says which for a
  slice). Counts are atomic, so slices cross threads like any string.
- **Never written in place.** A slice is never appended to in place, nor moved behind a header:
  appending to one copies it, even when it holds the buffer's only reference. A plain heap
  string is appended to in place only when its count is 1, which a live slice of it prevents.
  So neither a slice nor its parent ever writes bytes the other can see.
- **Header fields.** A slice has no breadcrumbs of its own: the buffer's describe the whole text,
  and positions in a non-ASCII slice are translated by scanning from its nearer end and the
  per-thread cursor (remembering a position in a slice sets the buffer's `crumbs` field to 1
  unless it has a table, so freeing the buffer bumps the epoch). The buffer's `lone` counts the
  whole buffer, so a slice reads it only as "none" when it is 0, and otherwise counts its own
  text when asked, without recording it.
- **Generated code** needs no change for slices and must not assume more: a slice tests as a
  heap string (`(int64_t)w2 > 0`), its `w0`/`w1` read as for any heap string, and dropping it
  calls `velt_rt_str_drop`. `w2` of a heap string is not its capacity, and its count is not at
  `w0 - 8`: only the runtime finds the buffer.
- A buffer with count > 1 is never written. The builder (§12.1 of rt_abi_async.md) appends in
  place only to an inline string with room or a plain heap string (not a slice) whose buffer has
  count 1 (of the right layout).
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
  calls `velt_rt_str_char_code_at`. The byte length (`Buffer.byteLength(s)`, intrinsic
  `StrByteLen`) is read inline too: `byte23 ≥ 0x80 ? byte23 & 0x1f : w1 & 0xffffffff`. Strings
  are passed to the runtime by pointer for everything else. Test a form through byte 23: inline
  appends write single bytes into `w2`, and reading `w2` as a word right after would stall on
  store forwarding.
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
  heap buffers), then `blocks=<allocated>/<freed>` and `channel leftovers=<n>` (values still
  queued in channels when `main` returned, dropped by the runtime then; rt_abi_async.md §2.2).
  Release runtimes compile the counters out; to count optimized code, link a
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
| `velt_rt_str_clone` | `(const VeltStr* s, VeltStr* out)` | a copy: bitwise, plus count +1 for heap strings (never a deep copy); generated code calls it for heap strings only (`(int64_t)w2 > 0`) and copies the three words of any other |
| `velt_rt_str_own` | `(const VeltStr* s, VeltStr* out)` | like `str_clone`, but a static-form string (which may borrow memory, e.g. a JSON key pointing into the parsed text) is copied into an inline or heap string |
| `velt_rt_str_drop` | `(VeltStr* s)` | count −1 for heap strings (frees at 0), then zeroes `*s`; generated code calls it for heap strings only and zeroes any other itself |
| `velt_rt_str_cmp` | `(const VeltStr* a, const VeltStr* b) -> int32_t` | -1 / 0 / 1 in UTF-16 code-unit order (`<`, `sort()`; #377 phase 2b): `memcmp` for two ASCII strings, else byte order corrected where it differs from code-unit order (design/strings.md "The ordering rule") |
| `velt_rt_str_char_code_at` | `(const VeltStr* s, int64_t i) -> int64_t` | the UTF-16 code unit at `i` (a supplementary character gives its high or low surrogate), -1 out of range; generated code calls it for non-ASCII strings only |
| `velt_rt_str_byte_length` | `(const VeltStr* s) -> uint64_t` | the UTF-8 length: the stored byte length, O(1) (generated code reads it inline, above) |
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

**Debug check of captured variables' cells** (#916). A captured variable that a closure assigns
lives in a counted *cell* (`LocalDef::boxed`), which must never be used by two tasks: the
transfer glue gives a value crossing to another task cells of its own, and sema rejects the
shapes that would share one (#901). Debug programs built by a debug compiler (as every debug
golden run is; `LowerOptions::cell_checks`) also report each cell to the debug runtime, which
records its owning **task** (`velt_rt_task_id`) in a side table keyed by the cell's address. The
owner is a task, not an OS thread: a task (with its local promises) may resume on another worker,
which is not a race. The first use by a task other than the owner prints `velt debug-cells:
captured variable <name> was used by two tasks: …` (both tasks and their threads, and the fix:
`shared(...)`) to stderr and aborts. A cell made outside any task is claimed by the first task
that uses it; uses outside any task (runtime clean-up, an HTTP request's set-up) are not checked. Release builds emit none of these
calls and the cell layout is the same; the release runtime keeps the symbols as empty functions
(and so does the WebAssembly runtime, which has one thread).

| Symbol | Signature | Emitted |
|---|---|---|
| `velt_rt_cell_new` | `(void* cell, const VeltStr* name)` | a new cell of variable `name`: owned by the current task (unowned outside a task) |
| `velt_rt_cell_use` | `(void* cell)` | before each count change of a cell, and on entry to a closure that assigns the captured variable: claims an unowned cell, aborts if another task owns it |
| `velt_rt_cell_give` | `(void* cell)` | the transfer glue moves the only reference to another task: unowned until the receiver uses it |
| `velt_rt_cell_copy` | `(void* cell, void* from)` | the transfer glue's fresh copy of the shared cell `from`: unowned |
| `velt_rt_cell_free` | `(void* cell)` | before the cell's last reference frees it |

## Weak references
`velt_rt` exports these functions (`src/weak/`); the compiler lowers `WeakMap`, `WeakSet` and
`WeakRef` (std/prelude/weak.vlt, `velt_vir` lower/weak.rs) to them, and `weak T` fields (#11)
will use them too. Design: [weak-refs.md](../design/weak-refs.md).

- **Count word.** Bit 63 (`RC_WEAK`) of a counted object's count is set while the object is weakly
  held; the count proper is bits 0..62. Retain is unchanged (`count += 1`).
- **Release** of a *weak-capable* type (decided program-wide: weak keys, `WeakRef` targets, `weak`
  targets and every type reachable from a weak map's key and value types): `c = count; if (int64_t)c
  > 1 { count = c - 1 } else if c == 1 { drop; free } else if velt_rt_weak_release(obj) { drop;
  free }` (`RC_WEAK` is the sign bit, so the shared path keeps one compare). Other types release as
  today.
- **Trace glue** per counted type that can be a weak key or value, or be reached from one:
  `void trace(uint8_t* obj, VisitFn visit, void* ctx)` calls `visit(ctx, child, child_trace)` once
  for each counted reference `obj` owns (`child_trace` may be null: opaque). Map values are
  retained with `void retain(uint64_t value)` and released with `void release(uint64_t value)`.
  A trace glue may leave references out (the target then looks referenced from outside and
  stays alive: a leak at worst) but must never report a word that is not a counted reference
  `obj` owns, or report one twice: the trial would count it as internal and could free a live
  object. So the glue of a type whose objects can have more fields than it knows (a class in a
  hierarchy with a vtable, an interface value, a function value's environment) is null, or
  dispatches on the object's dynamic type; Velt's is null for those today.
- **Map values.** A value word of a map with `value_release` is 0 (`null`, `undefined`) or a
  counted object pointer. The compiler stores a value whose type is a counted object (or
  `T | null` of one) as its pointer, a value without drop glue that fits in 8 bytes (numbers,
  `bool`) as its bits with no value glue, and any other value (strings, unions, tuples, function
  values) in a counted box of its own, `[count][value]`, whose glue retains and releases the box
  (the box is weak-capable) and traces the value in it. The runtime never retains, releases or
  traces a 0 value.
- **Threads.** Handles (`MapId`, `RefId`) are nonzero `uint32_t`s (generated code keeps 0 for
  "not created yet"), valid on every thread until dropped: the side table is one per process,
  behind a lock the runtime takes only in these functions, so a task that resumes on another
  worker, or a value reached through a `shared` `Mutex`, keeps its entries. The compiler keeps
  `WeakMap`, `WeakSet` and `WeakRef` values out of transfers (sema rejects them, or the transfer
  glue panics): a handle is used by one task at a time. Weak-capable types are never atomic. An
  object with `RC_WEAK` set and no record is an ICE (`velt_rt_weak_release` panics before it
  changes the count). Trace glue runs with the table locked and must only call `visit`.
- **Compiler rules** for weak-capable types: a uniqueness test compares the whole count word with
  1 (a weakly held object is never unique), and every site that decrements a count directly
  (`c - 1`: `lower/rc.rs`, `lower/glue/drop_chain.rs`, `lower/glue/transfer.rs`,
  `lower/glue/transfer_env.rs` in `velt_vir` today) uses the release sequence above instead.

| Symbol | Signature | Semantics |
|---|---|---|
| `velt_rt_weakmap_new` | `(TraceFn key_trace, RetainFn value_retain, ReleaseFn value_release, TraceFn value_trace) -> uint32_t` | a new map; null `value_retain` and `value_release` (both or neither) mean values are plain words (`WeakMap<K, number>`, `WeakSet`), a null `value_trace` that values never refer to keys (a `value_trace` needs value glue) |
| `velt_rt_weakmap_set` | `(uint32_t m, uint8_t* key, uint64_t value)` | takes over the value's reference; the key is not counted; releases a replaced value |
| `velt_rt_weakmap_get` | `(uint32_t m, uint8_t* key, uint8_t* found) -> uint64_t` | the value, counted (retained with `value_retain`; the caller releases it), or 0; `*found` = 1 if present |
| `velt_rt_weakmap_has` | `(uint32_t m, uint8_t* key) -> uint8_t` | |
| `velt_rt_weakmap_delete` | `(uint32_t m, uint8_t* key) -> uint8_t` | 1 if there was an entry; its value is released |
| `velt_rt_weakmap_drop` | `(uint32_t m)` | releases every value |
| `velt_rt_weakref_new` | `(uint8_t* obj) -> uint32_t` | does not count `obj`; bit 0 of `obj` may be set (a typed proxy, the object being `obj` with it clear), and `deref` returns the word as given |
| `velt_rt_weakref_deref` | `(uint32_t r) -> uint8_t*` | the target with count + 1, or null once freed |
| `velt_rt_weakref_drop` | `(uint32_t r)` | |
| `velt_rt_weakmap_set_traced` | `(uint32_t m, uint8_t* key, uint64_t value, TraceFn key_trace)` | `velt_rt_weakmap_set` with the key's own trace glue instead of the map's (keys of a map whose key type is `object` differ in type; the map is made with a null `key_trace`) |
| `velt_rt_weakref_new_with` | `(uint8_t* obj, uint64_t word) -> uint32_t` | `velt_rt_weakref_new`, keeping `word` with the slot (the descriptor of an `object` target) |
| `velt_rt_weakref_word` | `(uint32_t r) -> uint64_t` | the word given to `velt_rt_weakref_new_with` (0 for `velt_rt_weakref_new`), also after the target was freed |
| `velt_rt_weak_release` | `(uint8_t* obj) -> uint8_t` | cold release of an object with `RC_WEAK` set, given the caller's reference: 1 = it was the last one (the object has left every map and `WeakRef`; the caller drops and frees it), 0 = the count was decremented (and an ephemeron cycle the object belonged to may have been freed); panics (ICE) when the object has no record |

## Symbols [additive]

A `symbol` (HIR `TyKind::Symbol`, VIR `Ptr`) is the address of a record, never null, compared by
identity (equality, hashing):

```text
struct SymbolRec { desc: VeltStr /* empty without a description */, key: u64 }   // 32 bytes
```

`key` (without bit 63) is 0 for a record `velt_rt_symbol_new` made, 1 for a record of the registry
(`Symbol.for`), and 2 and up for another read-only record the compiler emits
(`Intrinsic::SymbolStatic`; its id + 2, so no two records have the same bytes); bit 63 is set when
the symbol has a description (`Symbol("")` has one, `Symbol()` has none). The compiler emits the
registry's records of module constants `const R = Symbol.for("r")` too (`key` 1, one per key, the
description being the key, in the static string form), and lists them in a read-only table
(`Intrinsic::SymbolRegistry`): their addresses, one per 8-byte slot, then a null slot. Records are
never freed; one made at run time is one allocation. On wasm32 each table address takes the
first half of its 8-byte slot. Functions (`std/symbol.vlt`; `string` parameters by pointer, a
`string` result through an out pointer):
- `velt_rt_symbol_new(desc: *const VeltStr, described: u8) -> *const SymbolRec`: `Symbol(desc)`
  (`described` = 0: `Symbol()`, `desc` unread); keeps a reference to `desc`.
- `velt_rt_symbol_for(key: *const VeltStr, table: *const RegistrySlot) -> *const SymbolRec`: the
  process-wide registry's record for `key`. On first use of a key it is the record in `table`
  whose description is `key` (`table` may be null), else a new one; a lookup does not allocate.
- `velt_rt_symbol_registered(s) -> u8`, `velt_rt_symbol_described(s) -> u8`,
  `velt_rt_symbol_description(s, out: *mut VeltStr)` (`""` without a description).
- `velt_rt_strbuf_push_symbol(buf: *mut VeltStrBuf, s)`: appends `Symbol(<description>)`
  (`console.log`, `String(s)`).

## CPU check [additive]

`velt_rt_check_cpu(required: u32)`: x86_64 code built for more than the baseline CPU
(`--cpu`, docs/tooling/platforms.md "CPU") calls it first thing in `velt_main`, after the
runtime's start-up (baseline code) and before any code that may use the features. `required` is
a mask of features, by bit: 0 SSE3, 1 SSSE3, 2 SSE4.1, 3 SSE4.2, 4 POPCNT, 5 CMPXCHG16B, 6 AVX,
7 AVX2, 8 BMI1, 9 BMI2, 10 FMA, 11 LZCNT, 12 MOVBE, 13 F16C, 14 AVX512F, 15 AVX512BW,
16 AVX512CD, 17 AVX512DQ, 18 AVX512VL, 19 AVX512VBMI, 20 AVX512BITALG
(`velt_codegen_cl::X86_FEATURES`; x86-64-v2 is bits 0–5, v3 0–13, v4 0–18). When the CPU lacks
one, it prints to stderr ``error: this program was built for <level> (<features>); this CPU
lacks <missing>; rebuild it with `--cpu <lower>` `` (`<lower>`: the highest level the CPU has,
or `baseline`) and exits with code 1. `VELT_CPU_DISABLE` (comma-separated feature names, any case) removes features from what the CPU
has, to test it.

## SIMD [additive]

A SIMD value (HIR `TyKind::Simd`, the ten vector and four mask types) is a VIR `V128`: 16 bytes,
8-byte aligned in memory (loads and stores are unaligned), passed and returned by value between
Velt functions. Lanes are in memory order: lane 0 is the lowest-addressed bytes, as in an array.
A mask lane is all ones or all zeros.

The operations are externs `velt_simd_<type>_<op>` (`velt_simd_u8x16_add`,
`velt_simd_mask8x16_any`) declared by `std/simd/*.vlt` (`velt:simd`). **No runtime defines
them**: `velt_vir::simd` gives each symbol's operation and VIR signature (`SimdCall`), the
verifier rejects a `velt_simd_*` extern that is not one, and both backends emit every call
inline. They never cross the C ABI, so no target's vector calling convention is involved.
`load` and `store` take the array (a pointer to its header) and an element index, and do not
check it: the prelude's methods do. `load_partial` and `store_partial` take a count of elements
too, below the lane count, and touch only those elements. `load_optimistic` [additive] is `load`
from an array of `Atomic` words other threads may be storing to (`T.loadOptimistic`): LLVM
emits a plain vector load, without an alias tag, and `freeze`s it, so a lane read while a store
raced it is some value rather than `undef` and nothing computed from it is undefined; Cranelift
emits a plain load. Correctness does not depend on how the hardware splits the load: a lane no
thread wrote during the load is the word, whatever the width of the accesses, and a lane a store
raced is rejected by the latch check whatever its bytes are (arm64 happens to read a 128-bit load
as two single-copy-atomic 64-bit reads; x86-64 documents no such guarantee for a 16-byte SSE
load; Velt's wasm targets have no threads).

What the latch check relies on: a racing plain load reads from no store, so in LLVM's model
nothing makes the version re-read see the writer's lock. It works, as Linux's seqlock and
crossbeam's `AtomicCell` do, because the load stays where the source puts it, between the
latch's acquire load and its acquire fence, and the hardware keeps that order (x86-64 TSO; on
arm64 the fence is `dmb ishld`, before the re-read). So no backend or VIR pass may hoist a
`load_optimistic` out of a loop, merge two of them, or move one across an atomic operation or a
fence. LLVM and Cranelift treat acquire loads and fences as clobbers of all memory, and velt_opt
moves no call.

`velt_simd_prefetch(void* addr)` (`-> void`, additive) starts loading the cache line at `addr`
for a read soon, as a hint: LLVM emits `llvm.prefetch` (read, every cache level, data), which
never faults; Cranelift emits nothing. Std reaches it through `__intrinsic_array_prefetch(xs, i)`
(HIR `Intrinsic::ArrayPrefetch`, the address of element `i`, not bounds-checked).

A shuffle is `velt_simd_<type>_shuffle_<i0>_<i1>…`, one lane index per lane spelled in the symbol
(`velt_simd_u32x4_shuffle_1_0_7_6`; indices below twice the lane count, a lane of the first
argument then of the second), so the mask is a constant wherever the call is: `(a, b) -> v`. No
std file declares one: sema checks the literals of `T.shuffle(a, b, …)` and lowering declares the
extern (HIR `Intrinsic::SimdShuffle`). `velt_simd_<type>_reverse_bytes` (16-, 32- and 64-bit
integer lanes) reverses the bytes within each lane.

**CPU extensions (#1330).** `velt_simd_u32x4_<op>` for the SHA instructions (`sha1c`, `sha1p`,
`sha1m`, `sha1h`, `sha1su0`, `sha1su1`, `sha256h`, `sha256h2`, `sha256su0`, `sha256su1` on
aarch64; `sha256rnds2`, `sha256msg1`, `sha256msg2`, `sha1rnds4_<0-3>`, `sha1nexte`, `sha1msg1`,
`sha1msg2` on x86-64; `velt_vir::simd::ShaOp`) are the instructions themselves: a caller runs one
only where `velt_simd_has_sha_arm()` / `velt_simd_has_sha_x86()` (`-> bool`) is true. On the
architecture with the extension that check reads the runtime's flag byte, otherwise (another
architecture, wasm, Cranelift) it is `false` and the operation, dead behind it, gives zeros.
The runtime exports the flags, set in `velt_rt_start` (`init`) before any Velt code runs and never
changed after (so generated code reads them as invariant); `VELT_SIMD_DISABLE=sha` leaves them 0:

```c
uint8_t velt_cpu_sha_arm;  /* 1: aarch64 with the ARMv8 SHA-1/SHA-256 instructions */
uint8_t velt_cpu_sha_x86;  /* 1: x86-64 with SHA, SSSE3 and SSE4.1 */
```

LLVM gives a function that calls one of these operations the extension's own feature (`+sha2`,
`+sha`), which it uses for nothing else; a *gated* function (VIR `Function::target`, HIR
`Program::cpu_targets`, from std's `__intrinsic_target("sha-arm" | "sha-x86")`, every call of
which sema keeps behind the check) gets the extension's whole set (`+sha,+ssse3,+sse4.1` on x86),
and the check of its own extension (`velt_simd_has_<ext>`) is the constant `1` there, so the
emulation behind it is dead code. The ordinary functions it calls that check that extension
(the `velt:simd` methods), themselves or through their callees, are cloned for it by `velt_opt`
with the check `true`, so this holds whether or not they would be inlined. A gated function is never inlined into an ordinary one or one
gated on another target (`velt_opt`, and `noinline` on the call for LLVM); ordinary code may be
inlined into a gated function.

## Atomics [additive]

The atomic operations (docs/internals/design/atomics.md) are externs
`velt_atomic_<op>_<bits>_<order>` on the address of a naturally aligned word, which lowering
calls for `Atomic<T>` (std/prelude/sync.vlt) and for `shared<int>`'s `add`, `get` and `set`
(rt_abi_async.md §9): `load`, `store`, the read-modify-writes `xchg add sub and or xor max min umax
umin` (`max`/`min` compare signed, `umax`/`umin` unsigned; `add`/`sub` wrap), and
`cas`/`casweak` with a failure ordering after the ordering (`velt_atomic_cas_64_acqrel_acq`),
with `bits` 8, 16, 32 or 64 and the orderings `rlx acq rel acqrel seqcst` (a load or a failure
ordering never `rel`/`acqrel`, a store never `acq`/`acqrel`). Also `velt_atomic_fence_<order>`
(never `rlx`) and `velt_atomic_spin_hint`. **No runtime defines them**, as for SIMD:
`velt_vir::atomic` gives each symbol's operation and VIR signature (`AtomicCall`), the verifier
rejects a `velt_atomic_*` extern that is not one, and both backends emit every call inline.
Values are passed as unsigned integers of the word's width (`U8` … `U64`; a `bool` is a `U8`
0 or 1); `load` and the read-modify-writes give the word (the *previous* value), `cas` the
previous value and `casweak` a `Bool`, whether it swapped (it may fail spuriously).

| Operation | VIR signature | LLVM | Cranelift |
|---|---|---|---|
| `load` | `(ptr) -> uN` | `load atomic iN, ptr, align N/8` with the ordering | `atomic_load` |
| `store` | `(ptr, uN)` | `store atomic` | `atomic_store` |
| `xchg` … `umin` | `(ptr, uN) -> uN` | `atomicrmw` | `atomic_rmw` |
| `cas` / `casweak` | `(ptr, uN expected, uN v) -> uN` / `-> bool` | `cmpxchg [weak]` with both orderings | `atomic_cas` (+ a compare) |
| `fence_<order>` | `()` | `fence` | `fence` |
| `spin_hint` | `()` | `llvm.aarch64.isb(15)` / `llvm.x86.sse2.pause`, nothing elsewhere (Rust's `spin_loop`) | nothing |

Orderings map one to one to LLVM's (`monotonic acquire release acq_rel seq_cst`). On Apple
arm64 (LSE and RCpc in every CPU) and x86-64, release builds get the instructions Rust's
`core::sync::atomic` gets (`ldapr`, `stlr`, `ldadd`, `casal`; `mov`, `lock xadd`). On arm64
Linux (generic armv8-a) clang compiles with `-moutline-atomics`, as Rust does: a
read-modify-write calls an `__aarch64_*` helper (libgcc's or compiler-rt's, the one in
`velt_rt` included) that uses LSE when the CPU has it; `max` and `min` have no helper and stay
LL/SC loops, as in Rust. Cranelift's
atomics are all sequentially consistent, which is correct for every ordering. On wasm without
threads they are plain loads and stores. `velt_rt_thread_yield()`
(a real runtime function, rt_abi_async.md §9) is what `velt:sync`'s `Backoff` yields with.

## Thread-local values [additive]

A module constant `new ThreadLocal<T>(init)` (std/prelude/sync.vlt; `T` a number type or `bool`,
`init` a literal) is one value per thread. Lowering calls `velt_tls_get_<id>_<bits>_<init>`
(`() -> uN`, this thread's value) and `velt_tls_set_<id>_<bits>_<init>` (`(uN)`): `id` is the
64-bit FNV-1a hash of the constant's qualified name (decimal; `velt_vir::tls::stable_id`, so it
stays the same when other declarations change), `bits` 8, 16, 32 or 64, and `init` the starting
word in lowercase hex. Values are passed as unsigned integers of the width, as for the atomics (a
float's bits, a `bool` as 0 or 1). **No runtime defines them**: `velt_vir::tls` gives each
symbol's meaning and VIR signature (`TlsCall`), and each backend implements them.

| Backend and target | Storage | A call |
|---|---|---|
| LLVM (every target) | `@velt_tls_data_<id> = hidden thread_local(initialexec) global iN init`, defined by the unit that defines `velt_main` and declared by the others | a load or store through `llvm.threadlocal.address` |
| Cranelift on ELF, Mach-O, x86-64 COFF | a thread-local data object `velt_tls_data_<id>` (`tls_model` `elf_gd`, `macho`, `coff`) | a load or store through `tls_value` |
| Cranelift JIT (`velt dev`), arm64 COFF | an operating system thread key, made the first time it is used, in `velt_tls_key_<id>` (the key plus one): defined by the object, or, in `velt dev`, a slot the session owns and every version imports, so all versions use one key | a call of the extern, which the module defines on `pthread_getspecific`/`pthread_setspecific` (`TlsGetValue`/`TlsSetValue` on Windows); the key holds the word XOR `init`, so a thread that never set it reads `init`. A failed key creation traps rather than use another key |

LLVM strips thread-local globals to plain ones for single-threaded WebAssembly.

## Module state [additive, #76]

```c
struct ModuleDesc {
  uint64_t size, align;
  void (*init)(void *state);
  void (*drop)(void *state);
  const char *names;   /* each slot's name, NUL-terminated, in slot order */
  void (*rest)(void *state); /* the root file's slots, or NULL (additive, #1438) */
};
void *velt_rt_module_state(const struct ModuleDesc *desc, uint64_t slot);
void *velt_rt_module_block(const struct ModuleDesc *desc);
void velt_rt_module_ready(void *state, uint64_t n);
```

`velt_rt_module_state` returns the process's module state block, for an access to slot `slot`:
allocated zeroed with `size`/`align` and filled by `init` on the first call (the block is current
while `init` runs). `init` calls `velt_rt_module_ready(state, i + 1)` after it stored slot `i`
(`u64::MAX` after the last slot); it stops before the root file's slots, which the root file's
`main` initializes where its statements reach them, on the block `velt_rt_module_block` returns
(the same block, allocated and initialized the same way, without the slot check). An access to a
slot whose initializer has not run yet (through a function an earlier initializer or statement
calls) prints `Uncaught ReferenceError: Cannot access '<name>' before initialization` and exits
with code 1. The block is not dropped while the program runs (only at exit under the debug
runtime's `VELT_RC_STATS=1`, for the leak counters, and only once every slot is initialized). A
call with a different descriptor than the block was made with (code swapped in by `velt dev`)
gets a block of its own, which `rest` (when not null) completes after `init`, as that code's
`main` does not run again.

The call takes the process-wide *module lock* unless the thread holds it: inside a task until
the end of the poll, and every later poll of that task takes it at its start; outside a task (a
synchronous `main`) until `velt_rt_block_on` or until `main` returns. A program that never
calls it pays nothing per task (a process-wide flag the first call sets). `velt_rt_mutex_lock`
keeps the module lock while it blocks; taking the module lock while the thread holds a `Mutex`
stops with an `ICE:` panic (sema rejects module state in a `with` callback). On WebAssembly (one
thread) there is no lock.

Within one call of a compiled function (until the poll ends, or until `velt_rt_block_on` or
`main` returns), a successful `velt_rt_module_state(desc, s)` stays valid for every slot `<= s`:
the same block, still initialized up to `s`, with the lock still held. The compiler reuses it
instead of calling again (`velt_opt` `module_state`, #1571), so a runtime change must keep this
[additive, #1571].

## Array length limit [additive, #1572]

An array (`{ data, len, cap }`) holds at most `MAX_ARRAY_LEN` = 2^52 - 1 elements, and its
buffer at most 2^52 - 1 bytes (`cap * size(T)`), so a byte size never wraps. Generated code
checks it where it chooses a length (`velt_vir` `lower/array`: `with_capacity`, `reserve`, an
array adopted from native code, and the growth of an array of zero-sized elements);
`velt_rt_realloc`, which generated code calls only to grow an array, refuses a `new_size` past it
(`velt_rt_alloc` does not check: it is on every `new`); the runtime checks the length in
`VeltBytes::from_vec`, `VeltArray::from_vec` and `VeltStrArray::from_vec` before handing a Rust
`Vec` over. Past it, both stop with
`panic: RangeError: Invalid array length: an array holds at most 2^52 - 1 elements, and its elements at most 2^52 - 1 bytes`
(exit code 101). Optimizations may rely on it: `velt_opt`'s `numrep` takes an array's `len` to be
at most 2^53 - 3 (all an index running to `len` inclusive needs). A typed array's byte length is
below 2^52 for the same reason (its buffer is a `u8[]`). `velt_rt_bytes_zeroed(n)` checks `n`
against the limit too. A buffer within the limit that the target cannot allocate is out of
memory (`velt_rt_alloc` above), never a smaller block: on wasm32 a byte size past 2^31 - 1
stops the program there, in `velt_rt_realloc`, in `velt_rt_bytes_zeroed` and in
`velt_rt_bytes_concat`.

## Array methods on a proxy [additive, #823]

```c
int64_t velt_rt_proxy_array_call(const void *target, int64_t op);
int64_t velt_rt_proxy_array_hole(const void *array, int64_t index, int64_t op);
```

The state of the generic array methods `velt:proxy/array` runs on a proxy of an array
(`p.push(…)`, `p.pop()`, …), per thread, so that the standard library keeps no module state.
Arrays are identified by the data pointer of the `any` holding them. Only a running method's
target is ever recorded: its proxy keeps it alive, so its address is not reused while the
method runs, and the records of a target go when its method returns.

`velt_rt_proxy_array_call(target, op)`: `op` 1 starts a method on a proxy of `target` (methods
nest), -1 ends it and answers the first hole it leaves in `target` (forgotten then; -1 for
none; the standard library makes a left hole a `TypeError`), 2 starts one of the method's own
trap calls (`has`, `get`, `set`, `deleteProperty` on the proxy), -2 ends it. Between trap calls
(the method's user callbacks) no trap call is running.

`velt_rt_proxy_array_hole(array, index, op)`: Node's empty slots, which Velt's arrays don't have.
`op` 0 answers whether element `index` is one (1 or 0); 1 records it, only during a trap call of
a method on `array` (answering whether it was one), else answers -1 and records nothing; 2
forgets it (whether it was one); 3 forgets the holes from `index` on (a shorter length; 0); 4
answers the first hole below `index`, or -1. std/proxy.vlt's `Reflect` records them (a delete,
the gap before an element written past the end) and reports them as absent; elsewhere both are
`TypeError`s. Both runtimes provide them (`velt_rt/src/proxy_array.rs`, compiled into
velt_rt_wasm too).

## Views [additive, #1485]

```c
uint8_t *velt_rt_view_get(const uint8_t *obj);
void velt_rt_view_put(const uint8_t *obj, const uint8_t *view);
void velt_rt_view_remove(const uint8_t *obj, const uint8_t *view);
uint8_t *velt_rt_array_view_get(const uint8_t *arr, uint64_t ty);
void velt_rt_array_view_put(const uint8_t *arr, uint64_t ty, const uint8_t *view);
void velt_rt_array_view_remove(const uint8_t *arr, uint64_t ty, const uint8_t *object);
void velt_rt_array_view_cold(void);
```

The live view of each object (velt_vir `lower/any_view.rs`): the `Proxy` that an `any` holding
an object of another type becomes when it is converted to an object type, so that converting an
object again while its view lives gives that view. `obj` is the data pointer of the `any`,
`view` the untagged `Proxy` object. The table counts neither: the view holds the object, and the
view's drop glue (before its fields drop) calls `velt_rt_view_remove`, so an entry lives exactly
as long as its view and its address is not reused meanwhile.

`velt_rt_view_get(obj)` answers the view, not counted (the caller retains it), or null;
`velt_rt_view_put(obj, view)` records a view just made, copied or transferred, unless `obj`
has one already; `velt_rt_view_remove(obj, view)` forgets the entry if it is `view`'s. A view's
transfer glue removes its entry before its `#target` may be replaced by a copy for another
thread and puts it under the new `#target` afterwards; its clone glue puts the copy under the
copied target. One table for the process, behind a lock (a task moves between
worker threads at an `await`). Both runtimes provide them (`velt_rt/src/view.rs`, compiled into
velt_rt_wasm too). Programs without views never call them.

The `velt_rt_array_view_*` functions keep a second table the same way for array views (velt_vir
`lower/array_view.rs`, hir_encodings.md "Array views"): from (`arr`, the data pointer of the
`any` holding the original array; `ty`, a number the compiler gives the view type, unique
within the program) to `view`, the view's box. A conversion asks it first and records the view
it makes; the view's drop glue removes the entry, and so does a transfer that turns the view
into a plain array in place. `velt_rt_array_view_remove` takes the view's view object (the
`__ArrayView`, untagged) and removes the entry if it is that object's view: if the entry's box
header holds `object | 1`. So the view's own box never reaches the runtime, and the backend
need not assume that a call may change a box of a view type.

`velt_rt_array_view_cold()` does nothing. The test before a loop versioned for a box
(`lower/view_unswitch.rs`) calls it when the box is a view, on the way into the loop's
original, and the LLVM backend declares it `cold` (and touching only memory compiled code
never reads), so that it treats the loop's copy for a plain array as the one that runs.

## Function properties [additive, #924]

```c
const uint8_t *velt_rt_fnprop_get(const uint8_t *env, const VeltStr *key);
void velt_rt_fnprop_set(uint8_t *env, const VeltStr *key, const uint8_t *value);
uint8_t velt_rt_fnprop_delete(const uint8_t *env, const VeltStr *key);
uint64_t velt_rt_fnprop_len(const uint8_t *env);
void velt_rt_fnprop_key(const uint8_t *env, uint64_t i, VeltStr *out);
uint8_t *velt_rt_fnprop_identity(const uint8_t *code);
uint8_t *velt_rt_fnprop_identity_of(const uint8_t *code);
```

Properties a program adds to a function value through `any` (velt_vir `lower/any_fn_props.rs`):
JavaScript functions are objects. The function's identity is its closure environment (a
program with writes through `any` that holds functions there gives every closure a counted
one), so the table maps the environment's address to its properties in insertion order, each
value an `any` (`{ data, table }`, two pointers). `get` answers the address of the value held
(borrowed: the caller shares it at once) or null; `set` takes the `any` at `value` and drops the
one it replaces after the store; `delete` answers 1 when there was one; `len` and `key` list the
names (`key` writes an owned string).

A function value without an environment (a declared function's) is one function in
JavaScript, whichever reference reaches it: `identity(code)` answers the environment the runtime
keeps for the function with that code address (an environment holding nothing, made on first
use, one more reference for the caller, which stores it into the value as its environment), and
`identity_of(code)` the same without making one or counting it (null when there is none). A
value converted to `any` gets it there, so `===`, `Map` keys and properties see one function.
These identities are module state (above): their counts and properties are not atomic, so both
functions take the module lock as an access of a module-level `let` does, and the task keeps
it in its polls from then on. A value holding one that goes to another task leaves it behind
(its transfer entry releases the reference and answers null), and finds it there again through
`identity_of`. The table holds its reference to the end; with the leak counters on
(`VELT_RC_STATS=1`), it is released at exit, as module state is.

The first `set` on an environment replaces its drop entry (its first word) and its transfer
entry (its third) with the runtime's. The drop calls the original and, with the last reference
(a count of 1 in front of the environment), first removes the entry, puts the original entries
back and drops the names and values. The transfer (the function enters another thread) moves a
unique environment and transfers each value in place; a shared one is copied (by its original
transfer entry, or as a new environment holding nothing), and the copy gets a share of each
value, transferred, so every thread has properties of its own, as a copied object has members
of its own. A tagged pointer (the many-threads check) goes to the original entry. Values are
dropped, shared and transferred through their table's slots (8 bytes each on every target),
never under the table's lock. One table for the process, behind a lock. Both runtimes provide
them (`velt_rt/src/fn_props.rs`, compiled into velt_rt_wasm too). Programs without a write
through `any` never call them.

## Output [M1]
`stream`: 1 = stdout (buffered, flushed at exit / before any stderr write / on `velt_rt_flush`), 2 = stderr.

| Symbol | Signature |
|---|---|
| `velt_rt_write_str` | `(uint32_t stream, const VeltStr* s)` |
| `velt_rt_write_i64` | `(uint32_t stream, int64_t v)` |
| `velt_rt_write_u64` | `(uint32_t stream, uint64_t v)` |
| `velt_rt_write_f64` | `(uint32_t stream, double v)` — `console.log` formatting (`1`, `1.5`, `1e+21`, `NaN`, `-0`) |
| `velt_rt_write_bool` | `(uint32_t stream, uint8_t v)` |
| `velt_rt_write_byte` | `(uint32_t stream, uint8_t b)` — used for `' '` and `'\n'` |
| `velt_rt_flush` | `(void)` |

`console.log(a, b)` lowers to: `write_<a>(1, a); write_byte(1, ' '); write_<b>(1, b); write_byte(1, '\n')`.
`f32` values are converted to `f64` before `write_f64`; narrower ints extended to 64-bit.

## Memory [M1]
| Symbol | Signature |
|---|---|
| `velt_rt_alloc` | `(uint64_t size, uint64_t align) -> void*` — never null for size > 0: a request it cannot satisfy stops the program with `panic: memory allocation of <size> bytes failed` (exit code 101; was an abort), as does `velt_rt_realloc`. On a 32-bit target (wasm32) a size past `isize::MAX` is such a request: the 64-bit size is never truncated |
| `velt_rt_realloc` | `(void* p, uint64_t old_size, uint64_t align, uint64_t new_size) -> void*` |
| `velt_rt_free` | `(void* p, uint64_t size, uint64_t align)` |

An array buffer (generated code's `T[]`) has its element's alignment, or 128 bytes when the
element's size is a multiple of 64 (`velt_vir` `buffer_align`): each element of such an array is
then whole 64-byte lines, and an aligned pair of them one 128-byte block (one cache line on
Apple's CPUs, an adjacent pair that others fetch together), as a B-tree node's two lines are.
Every allocation, growth and free of one buffer passes the same alignment.

`VELT_RT_DEBUG_ALLOC=1` with a **debug** runtime (programs linked against `<target>/debug/velt_rt`,
e.g. every golden run) checks every allocation of the process (generated code and runtime alike):
blocks carry a header and canaries and start filled with `0xCD`; a free checks for double
frees, foreign pointers, a size different from the allocation's and overwritten canaries
(overflow/underflow), fills the block with a poison word and keeps it in a quarantine (64 MiB) so
a use after free reads the poison instead of a reused block. The poison is an address in the
middle of 512 MiB of address space the allocator reserved, inaccessible, when it turned on, so a
read of a freed block aborts with `velt debug-alloc: use after free: <what> (address …)` where
the value is used: following a pointer read from it (an object field, a string's or an array's
buffer) faults inside the reservation, which a fault handler reports as `followed a pointer read
from a freed block`; printing a number read from it, or making a string of it, is `a number
(an integer) read from a freed block` (the poison reads as a subnormal `f64`, so a
read-modify-write of a freed number changes its bytes); retaining or releasing a string whose
buffer was freed is `a string whose buffer was freed`. A value only computed from (or a
`bool`/32-bit field read from) a freed block goes unnoticed until it reaches one of those. A
write after free is detected when the block leaves the quarantine, or when the program ends
(`main` returned or `velt_rt_exit`) for the blocks still in it. If the reservation fails the
poison is the byte `0xA5` (a tiny `f64` and a non-canonical pointer, which faults without a
message). Allocation sites are not recorded. The first violation prints `velt debug-alloc: <what>
(block …, size …)` to stderr and aborts. Release runtimes don't contain the check.

## Control [M1]
| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_panic` | `(const VeltStr* msg) -> noreturn` | flush stdout, print `panic: <msg>\n` to stderr, exit 101 |
| `velt_rt_exit` | `(int32_t code) -> noreturn` | flush stdout, exit |
| `velt_rt_uncaught_report` | `(void)` | called before an `Uncaught …` report is written (the report then ends the process): returns on the first thread to report (and on it again); any other thread waits until the process ends, so one report is printed (#1497). (additive) |
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
  `velt_rt_bytes_fill(b: u8[], byte: u8, start: i64, end: i64)`,
  `velt_rt_bytes_slice(b: u8[], start: i64, end: i64): u8[]`,
  `velt_rt_bytes_concat(a: u8[], b: u8[]): u8[]`. XOR masking (`xorMask`) is Velt code on SIMD
  vectors there, and UTF-8 validation (`utf8Valid`, the WebSocket reader) in
  `std/encoding/utf8.vlt`.
- For protocols written in Velt (`u8[]`'s `utf8Slice` in `std/prelude/bytes.vlt`;
  `std/websocket/bytes.vlt`), on a range `[start, end)` (relative indices) so no slice is made:
  `velt_rt_bytes_range_to_str(b: u8[], start: i64, end: i64): IoResult<string>` (`INVALID_DATA` if
  not UTF-8) and `velt_rt_bytes_range_to_str_lossy(b: u8[], start: i64, end: i64): string` (an
  invalid sequence as U+FFFD, one per maximal invalid subpart): making a string is the runtime's.
  Writing a string's bytes and comparing them (`write`, `utf8Equals`) is Velt code that reads the
  string's bytes in place (intrinsics `StrByteAt`, `StrLoadU8x16`, `StrLoadPartialU8x16`).
- BigInt (`std/bigint.vlt`, handle `u64`, freed by `velt_rt_bigint_free`): `from_i64`, `from_f64`,
  `parse(s, radix)`, `clone`, `assign(dst, a)`, `neg(dst, a)`, `op(dst, a, b, op: u32): bool` and
  `op_i64(dst, a, k: i64, op: u32): bool` (false: division by zero or a negative or huge shift; `dst` unchanged), `cmp`/`cmp_i64` (−1/0/1),
  `to_i64`, `to_f64`, `to_string(a, radix)`. `op` codes are defined in `std/bigint.vlt`.
- Numbers: `velt_rt_f64_to_fixed(x: f64, digits: i64): string` (JS `toFixed` rounding),
  `velt_rt_f64_to_exponential(x: f64, digits: i64): string` (JS `toExponential`; `digits < 0`:
  omitted), `velt_rt_f64_to_precision(x: f64, precision: i64): string` (JS `toPrecision`),
  `velt_rt_f64_to_radix(x: f64, radix: i64): string` (JS `toString(radix)` with V8's digits, for
  a finite `x`; `radix` clamped to 2..36), `velt_rt_i64_to_radix(x: i64, radix: i64): string`
  and `velt_rt_u64_to_radix(x: u64, radix: i64): string` (exact integer digits) (additive),
  `velt_rt_math_umulh(a: u64, b: u64): u64` (high 64 bits of the product),
  `velt_rt_math_f64_bits(x: f64): u64` and `velt_rt_math_bits_f64(b: u64): f64` (IEEE 754 bit
  casts, `x.toBits()` / `f64.fromBits(b)`; both backends emit them inline).
- Binary stdio: `velt_rt_stdout_write_bytes(bytes: u8[])` (synchronous, ordered with
  `console.log`), `async velt_rt_stdin_read_all_bytes(): IoResult<u8[]>` and
  `velt_rt_stdin_read_all_bytes_sync(): IoResult<u8[]>` (hand over the runtime's buffer).
