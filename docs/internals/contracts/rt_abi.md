# Runtime C ABI (velt_rt) — CONTRACT

Implemented by `crates/velt_rt` (Rust, `extern "C"`, `#[no_mangle]`). Called by code emitted
from VIR (`vir::ExternFn` entries created by lowering). All pointers are 64-bit. `bool` is `u8`
(0/1). Integers narrower than 64 bits are sign/zero-extended by the caller to the parameter type
listed.

## Process entry
- The runtime defines the C `main(argc, argv)`, compiled out under `cfg(test)` (the unit-test
  harness has its own), in `velt_rt_host` (the `velt dev` JIT host) and in `velt_rt_shared` (the
  shared runtime of debug builds). It initializes the runtime, stores args, calls `velt_main`,
  flushes stdout, returns its result.
- Generated code exports `int32_t velt_main(void)` (created by lowering; returns 0 for `void`
  mains).
- `int velt_rt_start(int argc, char** argv, int32_t (*entry)(void))` does what `main` does with
  `entry` as the program's main. The shared runtime (debug builds) has no `main`: the compiler
  emits a small entry object whose `main` calls `velt_rt_start(argc, argv, &velt_main)`.

## Native libraries of packages
Contract: [native_abi.md](native_abi.md). Called by `velt_main` before the user `main`, once per
package with a native library:

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_native_api` | `() -> const VeltRtApi*` | the function table handed to `velt_native_init_<pkg>` |
| `velt_rt_native_check` | `(int32_t rc, const VeltStr* package)` | `rc != 0`: prints ``error: the native library of package `<name>` failed to start (code <rc>)`` to stderr, exits 1 |

## Strings
Strings are immutable values ([design/semantics.md](../design/semantics.md)): copying one never
copies its bytes.
```c
typedef struct { uint64_t w0, w1, w2; } VeltStr;   // size 24, align 8 (vir::STR_AGG); little-endian
```
The bytes are **canonical WTF-8**: UTF-8 that may also hold a lone surrogate as a 3-byte sequence
(`ED A0..BF xx`), where a surrogate pair is always stored as its 4-byte code point (so byte
equality is code-unit equality). Every value also carries its **UTF-16 length** (code units,
[design/strings.md](../design/strings.md)); a string is ASCII exactly when its unit count equals
its byte count. `length` and every position count code units.

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
- A **slice** is a heap string whose bytes lie inside another heap string's buffer, which
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
  `velt_rt_str_slice` (`slice`, `substring`, `charAt`, `at`, `s[i]`), `velt_rt_str_trim`,
  `velt_rt_str_trim_start`, `velt_rt_str_trim_end`, and `velt_rt_str_split` when its separator is
  not empty and cannot match half of a surrogate pair. They return a slice when the piece is too
  long to be inline, at least a quarter of the buffer's capacity, and the buffer is below 1 GiB
  (2^30 bytes); otherwise they copy (inline when short). Every other function copies the pieces it
  returns: `split("")` and `velt_rt_str_code_points` (pieces of one character, always inline), a
  split by a lone surrogate, and the regex functions (the pieces of `match`, `exec` and `split` are
  new strings, `VeltStr::from_vec`). In every function the whole string is shared as it is (count
  +1, same value), a piece of a static string borrows it, and a piece that ends between the halves
  of a surrogate pair is a copy (the half is re-encoded). So live slices pin at most four times
  their own size (a short piece of a huge string never keeps it alive), and a parser that consumes
  its input from the front (`rest = rest.slice(n)`) copies at most a third of its buffer's capacity
  in all: a third of the input when the capacity is its length, up to about two thirds when appends
  grew the buffer (capacity up to twice the length). Linear either way.
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
- Bytes enter a string through one runtime function (`VeltStr::push_wtf8`; every append,
  including `velt_rt_str_append` and the builder's pushes, ends there), which keeps the unit
  count, the lone count and the form in step in O(1) per append (geometric growth), and joins a
  high surrogate ending the string with a low one starting the appended text into the pair's
  4-byte code point (only when both sides have lone surrogates: units are unchanged, bytes and
  lone count shrink by 2). Code outside the runtime's string module never writes `w1`/`w2`.
- Generated code reads the length (code units) inline:
  `byte23 ≥ 0x80 ? (byte23 & 0x40 ? byte22 : byte23 & 0x1f) : (int64_t)w1 >> 32` (the high
  half by an arithmetic shift, exact because strings are below 2 GiB; a zero-extending read
  lets LLVM vectorize index loops badly). `charCodeAt(i)` tests the form: an inline string with
  bit 0x40 of byte 23 clear, or a static/heap string whose halves of `w1` are equal, is ASCII,
  and the byte at `i` is loaded inline (bounds-checked against the length); any other string
  calls `velt_rt_str_char_code_at`. The byte length (`Buffer.byteLength(s)`, intrinsic
  `StrByteLen`) is read inline too: `byte23 ≥ 0x80 ? byte23 & 0x1f : w1 & 0xffffffff`. std code
  reads the words themselves with `strWord0`/`strWord1`/`strWord2`, and the address of the bytes
  with `strDataAddress` (`byte23 ≥ 0x80 ? &value : w0`), the layout above being what they
  return. Strings are passed to the runtime by pointer for everything else. Test a form through
  byte 23: inline appends write single bytes into `w2`, and reading `w2` as a word right after
  would stall on store forwarding.
- Generated code also writes the inline form: std's intrinsic `strAsciiWords(w0, w1, w2,
  len)` makes an ASCII string of at most 23 bytes as three word stores, `w2`'s top byte replaced
  by `0x80 | len`. The bytes past `len` are 0, as in every inline string the runtime makes (std
  guarantees it; the words are compared and hashed whole).
- Invariants, checked by a **debug** runtime on every append (each appended piece is canonical
  WTF-8 with the unit and lone counts it is given, and the seam is canonical: O(piece), never a
  recount of the whole string) and in full after every operation by the runtime's own tests: the
  bytes are canonical WTF-8; the stored unit count is the text's; a non-ASCII heap buffer's
  `lone`, unless unknown, is its number of lone surrogates; an inline string fits its form, and
  has no lone surrogates when its `0x20` bit is clear.
- **Well-formed text and output**. A string is well-formed (valid UTF-16, so
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
- **Breadcrumbs** (`crumbs`; layout in "Breadcrumbs" below): for translating a code-unit index to a
  byte offset and back in a non-ASCII heap string of more than 64 units, the runtime builds on first
  use a table of `u32` byte offsets, one per 64th unit (top bit: that unit is the low half of the
  4-byte sequence at the offset), publishes it in the header with a compare-and-swap and frees it
  with the buffer. A buffer that is appended to keeps its table (the prefix never changes), and a
  translation extends it when needed without trusting the count (a count-1 string may be read by two
  threads at once without a retain): entries are atomics written before the table's length covers
  them, and a table without room is replaced by a published copy twice its size, the old one kept
  alive until the buffer is freed. ASCII strings translate in O(1), other strings (short, inline,
  static) by a scan. Every code-unit position the runtime takes or returns goes through it. Each
  thread also remembers its last two translations of non-ASCII strings of more than 64 units, heap
  (with a table) or static (address, `w1`, form, position); a translation near one steps from it, so
  sequential index loops decode one character per step (in a static string, any forward step unless
  the end is closer). Freeing or growing a buffer that has a table first bumps a global epoch, which
  forgets every remembered position in a heap string (a new string at the same address is never
  taken for the old one). Positions in static strings never expire: a static non-ASCII string of
  more than 64 units points at a literal (the JSON reader copies such a key instead of borrowing it,
  rt_abi_async.md §12.3), and so must any other producer of borrowed views.
- `VELT_RC_STATS=1` with a **debug** runtime prints one line to stderr at exit,
  `rc stats: retain=<n> release=<n> alloc=<n> free=<n> blocks=<allocated>/<freed> panics caught=<n>`
  (retain = increments, release = decrements of shared buffers, alloc/free = heap string
  buffers, blocks = every non-empty `velt_rt_alloc`/`velt_rt_free` and array buffer handed over,
  panics caught = panics stopped at a promise that catches them, each of which leaks).
  Release runtimes compile the counters out; to count optimized code, link a release-built
  program against the debug runtime (`VELT_RT_LIB` naming `<target>/debug/libvelt_rt.a`, or
  `velt_rt.lib` on Windows).
- `VELT_STDOUT_STATS=1` with a **debug** runtime prints `stdout stats: writes=<n>` to stderr at
  exit: how many times buffered stdout was written to the OS. Tests check buffering with it
  (piped output goes out in blocks: far fewer writes than lines) instead of timing a program.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_str_concat` | `(const VeltStr* a, const VeltStr* b, VeltStr* out)` | new string (an empty operand: the other one, shared) |
| `velt_rt_str_append` | `(VeltStr* s, const VeltStr* t)` | `s += t` on the owned string `*s`, whose old value is dead: in place when `*s` is inline with room or holds the only reference to its heap buffer (which grows geometrically); a static or shared `*s` is first copied into a buffer of its own. `t` may be `s` or lie in `*s`'s buffer. Emitted for `s += x`, `s = s + x` and `` s = `${s}${x}` `` on variables and fields (rt_abi_async.md §12.1) |
| `velt_rt_str_from_f64` | `(double v, VeltStr* out)` | JS `Number.prototype.toString` formatting (inline when short). Integers have no runtime function: their text is std's `intText` / `uintText` (std/lang/hooks/number_text.velt) |
| `velt_rt_str_from_bool` | `(uint8_t v, VeltStr* out)` | `true`/`false` (static) |
| `velt_rt_str_clone` | `(const VeltStr* s, VeltStr* out)` | a copy: bitwise, plus count +1 for heap strings (never a deep copy); generated code calls it for heap strings only (`(int64_t)w2 > 0`) and copies the three words of any other |
| `velt_rt_str_own` | `(const VeltStr* s, VeltStr* out)` | like `str_clone`, but a static-form string (which may borrow memory, e.g. a JSON key pointing into the parsed text) is copied into an inline or heap string |
| `velt_rt_str_drop` | `(VeltStr* s)` | count −1 for heap strings (frees at 0), then zeroes `*s`; generated code calls it for heap strings only and zeroes any other itself |
| `velt_rt_str_cmp` | `(const VeltStr* a, const VeltStr* b) -> int32_t` | -1 / 0 / 1 in UTF-16 code-unit order (`<`, `sort()`): `memcmp` for two ASCII strings, else byte order corrected where it differs from code-unit order (design/strings.md "The ordering rule") |
| `velt_rt_str_char_code_at` | `(const VeltStr* s, int64_t i) -> int64_t` | the UTF-16 code unit at `i` (a supplementary character gives its high or low surrogate), -1 out of range; generated code calls it for non-ASCII strings only |
| `velt_rt_str_byte_length` | `(const VeltStr* s) -> uint64_t` | the UTF-8 length: the stored byte length, O(1) (generated code reads it inline, above) |
| `velt_rt_str_hash` | `(const VeltStr* s) -> uint64_t` | hash of the bytes (`Map`/`Set` keys; fixed seed) |

## Counted objects
Compiled code only (no runtime functions): values the program shares
([design/semantics.md](../design/semantics.md#which-types-are-counted)) live in heap blocks
`[count: u64][value]` allocated with `velt_rt_alloc(8 + size, 8)`; the value pointer is the
block address + 8, so it is also what a borrow of the value passes (`T*`). Counts are plain
(non-atomic) — counted objects never cross threads — and are updated inline. The runtime only
ever sees values in their unboxed layout: an extern parameter or result whose type holds boxed
values crosses as an unboxed view (arguments) or is boxed after the call (results). Closure
environments on the heap are counted blocks too; the drop function in their header releases one
reference.

**Debug check of captured variables' cells**. A captured variable that a closure assigns
lives in a counted *cell* (`LocalDef::boxed`), which must never be used by two tasks: the
transfer glue gives a value crossing to another task cells of its own, and sema rejects the
shapes that would share one. Debug programs built by a debug compiler (as every debug
golden run is; `LowerOptions::cell_checks`) also report each cell to the debug runtime, which
records its owning **task** (its task id) in a side table keyed by the cell's address. The
owner is a task, not an OS thread: a task (with its local promises) may resume on another worker,
which is not a race. The first use by a task other than the owner prints ``velt debug-cells:
captured variable `<name>` was used by two tasks: …`` (both tasks and their threads, and the fix:
`shared(...)`) to stderr and aborts. A cell made outside any task is claimed by the first task
that uses it; uses outside any task (runtime clean-up, an HTTP request's set-up) are not
checked. A cell reached through module state (a closure stored in a module-level variable) is
the exception: a use by another task while that task holds the module lock (below) makes the
cell *guarded*, and from then on every use must hold the module lock; one that does not prints
``velt debug-cells: captured variable `<name>` is shared through module state, and task …
used it without the module lock`` and aborts. Release builds emit none of these calls and the
cell layout is the same; the release runtime keeps the symbols as empty functions (and so does
the WebAssembly runtime, which has one thread). Implemented in `velt_rt` `cell_owner.rs`.

| Symbol | Signature | Emitted |
|---|---|---|
| `velt_rt_cell_new` | `(void* cell, const VeltStr* name)` | a new cell of variable `name`: owned by the current task (unowned outside a task) |
| `velt_rt_cell_use` | `(void* cell)` | before each count change of a cell, and on entry to a closure that assigns the captured variable: claims an unowned cell, aborts if another task owns it |
| `velt_rt_cell_give` | `(void* cell)` | the transfer glue moves the only reference to another task: unowned until the receiver uses it |
| `velt_rt_cell_copy` | `(void* cell, void* from)` | the transfer glue's fresh copy of the shared cell `from`: unowned |
| `velt_rt_cell_free` | `(void* cell)` | before the cell's last reference frees it |

## Weak references
std's weak core (`std/lang/hooks/weak*.velt`) does this work. Lowering (`velt_vir`
`lower/weak.rs`, `lower/weak_object.rs`) reaches it through the `weak_*` hooks
(`crates/velt_common/src/std_hooks/weak_hooks.rs`), and std's `WeakMap`, `WeakSet` and `WeakRef`
(std/prelude/weak.velt, weak_set.velt, weak_ref.velt) reach their drop hooks through intrinsics.
The hooks are `prebuilt`: where the build links the core's object, each is a call of its entry
point `velt_hook_<hook>` (numbers and `bool`s by value), and the program defines the storage of
the object's thread-local values (`velt_tls_data_<id>`). Design:
[weak-refs.md](../design/weak-refs.md).

- **Count word.** Bit 63 (`RC_WEAK`) of a counted object's count is set while the object is weakly
  held; the count proper is bits 0..62. Retain is unchanged (`count += 1`).
- **Release** of a *weak-capable* type (decided program-wide: weak keys, `WeakRef` targets, `weak`
  targets and every type reachable from a weak map's key and value types): `c = count; if (int64_t)c
  > 1 { count = c - 1 } else if c == 1 { drop; free } else if weakRelease(obj) { drop; free }`
  (`RC_WEAK` is the sign bit, so the shared path keeps one compare). Other types release with the
  plain sequence.
- **Trace glue** per counted type that can be a weak key or value, or be reached from one:
  `void trace(uint8_t* obj, VisitFn visit, void* ctx)` calls `visit(ctx, child, child_trace)` once
  for each counted reference `obj` owns (`child_trace` may be null: opaque), each argument a
  `uint64_t` (a pointer widened: on wasm32 an indirect call must match the callee's signature). Map values are
  retained with `void retain(uint64_t value)` and released with `void release(uint64_t value)`.
  These are C functions the core calls through their addresses (`addressCall3`,
  `addressCallWord`); `visit` is a std function (`fnAddress`).
  A trace glue may leave references out (the target then looks referenced from outside and stays
  alive: a leak at worst) but must never report a word that is not a counted reference `obj` owns,
  or report one twice: the trial would count it as internal and could free a live object. So the
  glue of a type whose objects can have more fields than it knows (a class in a hierarchy with a
  vtable, an interface value, a function value's environment) is null, or dispatches on the object's
  dynamic type. Velt's walks an uncounted object's fields as its owner's (the owner holds the only
  reference). A class with a vtable's glue calls the glue in its object's class table (slot −8,
  which no class table uses otherwise: the walk of exactly that class's fields, or null when it
  refers to nothing). An interface value's glue compares its table with the tables built for classes
  implementing the interface and visits the data pointer with the matching class's glue; a boxed
  value's table matches none and is left out. A function value's glue dispatches on its
  environment's drop entry (header word 0, one function per closure instance): it visits the
  environment with the trace glue of the closure whose drop entry it is, among those made at the
  value's function type, and leaves out an environment that matches none (one the runtime made, or
  one whose drop entry a function property replaced, `fn_props.rs`). An environment's trace visits
  what its captures own and its captured variables' cells (counted `[count][value]` blocks, traced
  like a map value's box). In a program with such glue, environments and cells may be weakly held,
  and their releases take the three-way sequence below.
- **Map values.** A value word of a map with value release glue is 0 (`null`, `undefined`) or a
  counted object pointer. The compiler stores a value whose type is a counted object (or
  `T | null` of one) as its pointer, a value without drop glue that fits in 8 bytes (numbers,
  `bool`) as its bits with no value glue, and any other value (strings, unions, tuples, function
  values) in a counted box of its own, `[count][value]`, whose glue retains and releases the box
  (the box is weak-capable) and traces the value in it. The core never retains, releases or
  traces a 0 value.
- **Threads.** Handles of maps and `WeakRef`s are nonzero `uint32_t`s (generated code keeps 0
  for "not created yet"), valid on every thread until dropped: the core's table is one per
  process, behind a lock taken only in these hooks, so a task that resumes on another worker, or
  a value reached through a `shared` `Mutex`, keeps its entries. The compiler keeps `WeakMap`,
  `WeakSet` and `WeakRef` values out of transfers (sema rejects them, or the transfer glue
  panics): a handle is used by one task at a time. Weak-capable types are never atomic. An object
  with `RC_WEAK` set and no record is an ICE (`weakRelease` panics before it changes the count).
  Trace glue runs with the table locked and must only call `visit`.
- **Compiler rules** for weak-capable types: a uniqueness test compares the whole count word with
  1 (a weakly held object is never unique), and every site that decrements a count directly
  (`c - 1`) uses the release sequence above instead (`velt_vir` asks `Cx::weak_capable` at each:
  `lower/rc.rs`, `lower/drops.rs`, `lower/boxes.rs`, the drop and transfer glue).

| Hook | Signature | Semantics |
|---|---|---|
| `weakMapNew` | `(key_trace, value_retain, value_release, value_trace: usize) -> u32` | a new map; 0 `value_retain` and `value_release` (both or neither) mean values are plain words (`WeakMap<K, number>`, `WeakSet`), a 0 `value_trace` that values never refer to keys (a `value_trace` needs value glue) |
| `weakMapSet` | `(m: u32, key: usize, value: u64)` | takes over the value's reference; the key is not counted; releases a replaced value |
| `weakMapSetTraced` | `(m: u32, key: usize, value: u64, key_trace: usize)` | `weakMapSet` with the key's own trace glue instead of the map's (keys of a map whose key type is `object` differ in type; the map is made with a 0 `key_trace`) |
| `weakMapGet` | `(m: u32, key: usize) -> u64` | the value, counted (retained with `value_retain`; the caller releases it), or 0 |
| `weakMapHas` | `(m: u32, key: usize) -> bool` | |
| `weakMapDelete` | `(m: u32, key: usize) -> bool` | whether there was an entry; its value is released |
| `weakMapDrop` | `(m: u32)` | releases every value |
| `weakRefNew` | `(obj: usize) -> u32` | does not count `obj`; bit 0 of `obj` may be set (a typed proxy, the object being `obj` with it clear), and `weakRefDeref` returns the word as given |
| `weakRefNewWith` | `(obj: usize, word: u64) -> u32` | `weakRefNew`, keeping `word` with the slot (the descriptor of an `object` target) |
| `weakRefWord` | `(r: u32) -> u64` | the word given to `weakRefNewWith` (0 for `weakRefNew`), also after the target was freed |
| `weakRefDeref` | `(r: u32) -> usize` | the target with count + 1, or 0 once freed |
| `weakRefDrop` | `(r: u32)` | |
| `weakRelease` | `(obj: usize) -> bool` | cold release of an object with `RC_WEAK` set, given the caller's reference: true = it was the last one (the object has left every map and `WeakRef`; the caller drops and frees it), false = the count was decremented (and an ephemeron cycle the object belonged to may have been freed); panics (ICE) when the object has no record |

## Symbols

A `symbol` (HIR `TyKind::Symbol`, VIR `Ptr`) is the address of a record, never null, compared by
identity (equality, hashing):

```text
struct SymbolRec { desc: VeltStr /* empty without a description */, key: u64 }   // 32 bytes
```

`key` (without bit 63) is 0 for a record `velt_rt_symbol_new` made, 1 for a record of the registry
(`Symbol.for`, the compiler's or std's), and 2 and up for another read-only record the compiler emits
(`Intrinsic::SymbolStatic`; its id + 2, so no two records have the same bytes); bit 63 is set when
the symbol has a description (`Symbol("")` has one, `Symbol()` has none). The compiler emits the
registry's records of module constants `const R = Symbol.for("r")` too (`key` 1, one per key, the
description being the key, in the static string form), and lists them in a read-only table
(`Intrinsic::SymbolRegistry`): their addresses, one per 8-byte slot, then a null slot. Records are
never freed; one made at run time is one allocation. On wasm32 each table address takes the
first half of its 8-byte slot.

The registry is std's (`std/lang/symbol/registry.velt`): a process-wide map from key to symbol
behind a lock, which starts with the table's records and makes a record for a key seen first at
run time (32 bytes from the C library, outside the leak counts; its description words those of
the key the map keeps). std reads and writes records with `Intrinsic::SymbolAddress` (a
symbol's record address) and `Intrinsic::SymbolAt` (the symbol at an address), both unsafe and
std-only, at the offsets above (`RECORD_KEY_AT` = 24, `KEY_REGISTERED`, `DESCRIBED`), which a std
test pins against the compiler's records. Functions (declared in `std/lang/symbol/index.velt`; `string`
parameters by pointer, a `string` result through an out pointer):
- `velt_rt_symbol_new(desc: *const VeltStr, described: u8) -> *const SymbolRec`: `Symbol(desc)`
  (`described` = 0: `Symbol()`, `desc` unread); keeps a reference to `desc`.
- `velt_rt_symbol_described(s) -> u8`, `velt_rt_symbol_description(s, out: *mut VeltStr)`
  (`""` without a description).
- `velt_rt_strbuf_push_symbol(buf: *mut VeltStrBuf, s)`: appends `Symbol(<description>)`
  (`console.log`, `String(s)`).

## CPU check

std's start-up hook `cpuStart` (std/lang/hooks/cpu.velt, velt_common `std_hooks` `cpu_start`),
which `velt_main` calls right after `velt_rt_statics` and before any other Velt code, does the
check the runtime's `velt_rt_check_cpu` did until #3222 step 3 (its Rust, `velt_rt` `cpu.rs`, is
deleted), in every build for x86-64 and arm64, Cranelift's too (`#cpu-start` picks the
architecture's detection; on wasm it does nothing). The hook and everything it reaches run before
anything knows what the CPU has, so they are compiled for the architecture's baseline CPU
whatever `--cpu` says (`vir::Function::baseline`, `velt_vir::start_up`): LLVM gives them, and the
helpers of their module, clang's `x86-64` (on arm64 the target's default CPU) as `target-cpu`
with its features only; Cranelift compiles them with a second ISA for that CPU; the std cache
serves none of them. It computes the CPU's **feature word** (std/lang/cpu, velt_common
`cpu_features`): bit 0
SSE3, 1 SSSE3, 2 SSE4.1, 3 SSE4.2, 4 POPCNT, 5 CMPXCHG16B, 6 AVX, 7 AVX2, 8 BMI1, 9 BMI2, 10 FMA,
11 LZCNT, 12 MOVBE, 13 F16C, 14 AVX512F, 15 AVX512BW, 16 AVX512CD, 17 AVX512DQ, 18 AVX512VL,
19 AVX512VBMI, 20 AVX512BITALG, 21 SHA, 22 PCLMULQDQ, 23 AES (`velt_codegen_cl::X86_FEATURES`; a
feature whose registers the OS must save counts only where XCR0 says it does), from CPUID and
XGETBV on x86-64 (`velt_simd_cpuid` and `velt_simd_xgetbv`, "SIMD": inline assembly under
LLVM; under Cranelift, which has no instruction for either, a function of fixed SSE2 code each
that the module defines, velt_codegen_cl `cpuid.rs`); on arm64 bit 32 DOTPROD, 33 SHA2, 34 PMULL, 35 CRC32, 36 SHA3 (with SHA512), 37
FP16, 38 I8MM, 39 BF16, 40 AES, from `getauxval(AT_HWCAP, AT_HWCAP2)` on Linux, `hw.optional`
sysctls on macOS and `IsProcessorFeaturePresent` on Windows (`PF_ARM_SHA3_INSTRUCTIONS_AVAILABLE`
to `PF_ARM_V86_BF16_INSTRUCTIONS_AVAILABLE` for SHA3 to BF16, which an older Windows reports as
absent).
`VELT_CPU_DISABLE` (comma-separated names of this architecture's list, any case) clears bits, to
test the check.

It then compares the word with `velt_simd_cpu_required() -> u64`, the bits the program was built
for (`--cpu`, https://veltlang.com/docs/tooling/platforms/ "CPU"; x86-64-v2 is bits 0–5, v3
0–13, v4 0–18; on arm64 the mask over `velt_codegen_cl::ARM_FEATURES` shifted to bit 32, bit 0
DOTPROD). The driver fills that call in with a constant right before code generation (`veltc`
`driver/cpu_check.rs`, `--emit llvm` included; a function that reads it is never served from the
std cache). A feature is in the mask when generated code uses it (FEAT_FHM, FEAT_BF16 and
AVX512-BF16 are not: "SIMD"). When the CPU lacks a feature of the mask, it prints to stderr
``error: this program was built for <what>; this CPU lacks <missing>; rebuild it with
`--cpu <lower>` `` and exits with code 1 (`velt_rt_exit`). `<what>` is `<level> (<features>)`
when the mask is exactly an x86-64 level, else `an x86-64 CPU with <features>` (`an arm64 CPU
with DOTPROD`); `<lower>` is the highest level the CPU has, or `baseline` (always on arm64).
Finally `VELT_SIMD_DISABLE` (`sha`, `clmul`, `aes`) clears those instructions' bits on both
architectures, and the word is stored once with `velt_simd_cpu_word_set(word)`, which every
run-time check reads as invariant (`velt_simd_cpu_word`, "SIMD"). The LLVM backend keeps it in
`velt_cpu_word`, a `linkonce_odr hidden` `i64` that every object reading or setting it defines,
so a program has one; no runtime code is involved (a std static would be an `Atomic`, which
brings velt/sync's reclaimer into every program). The hook's reads of the operating system and of the two variables
are not input: a program that reaches nothing else still reaches nothing (`veltc`
`driver/outside.rs`, for `velt test`'s results and source generators).

Under Cranelift (and on wasm) `velt_simd_on_x86()` and `velt_simd_on_arm()` are `false`,
`velt_simd_cpu_word()` is 0 and `velt_simd_cpu_word_set` does nothing, so every run-time check is
false: no gated code runs there, though the requirement is checked. Code whose features hold a feature's mask (SHA with SSE4.1 and down
for `sha-x86`) takes its check (`velt_simd_has_<feature>`, "SIMD") as true; the LLVM backend folds
it.

## Math operations

The scalar math functions `velt_rt_math_*` (`sqrt floor ceil round trunc fabs f64_bits bits_f64
umulh to_int32 mul_int32 add_int32 clz32 clz64 ctz64 popcount64`) and `velt_rt_pow_i64` (`**` on
integers; on floats it is std's `powF64` hook, std/lang/hooks/pow.velt, which calls `pow` of
std/lang/math/pow.velt) are runtime functions
(velt_rt's `math.rs`, the reference for what each computes) that compiled code treats as
operations: `velt_vir::math::MathOp` gives each symbol's operation and VIR signature, the
verifier rejects one of these externs with another signature, and backends emit all but `pow`
inline (an instruction, or a helper whose slow path calls the runtime).
`velt_vir::math::effects` is the one list of what a runtime function may do to memory (none for
every math operation; read-only, read-caching or cold for some string and array functions),
which LLVM's declarations and `velt_opt` both read. `velt_opt` folds a call whose arguments are
constants (`MathOp::eval`, which a test of veltc checks against the runtime's functions), except
a double operation with a NaN argument or result, whose bits the hardware chooses. Lowering
emits a math operation as `Rvalue::Intrinsic(Intrinsic::Math(op), …)` (vir.rs invariant 12), a
statement that means the same as the call, and never as a direct call (the verifier rejects
one); the extern stays declared, and backends call it for `pow` and on ToInt32's cold path.

The math operations named `velt_math_*` have **no runtime function**: like the SIMD externs,
std declares them and every backend emits them inline, so no runtime defines them and nothing
may call them. `MathOp::eval` is their reference, and `MathOp::has_runtime_function` tells the
two kinds apart. They are `f32_bits bits_f32` (`x.toBits()` / `f32.fromBits(b)` on `f32`),
`f16_bits bits_f16 bf16_bits bits_bf16` (`(u16) -> u16`, the identity: an `f16` or `bf16` value
is its bits) and the f16/bf16 conversions, each 16-bit value its bit pattern in a `u16`:
`f16_to_f32(h: u16): f32`, `f32_to_f16(x: f32): u16`, `f64_to_f16(x: f64): u16`,
`bf16_to_f32(b: u16): f32`, `f32_to_bf16(x: f32): u16`, `f64_to_bf16(x: f64): u16`,
`i64_to_bf16(x: i64): u16` and `u64_to_bf16(x: u64): u16`. Widening is exact; narrowing rounds
to nearest even once, from the exact source (an f64 or an integer is not rounded through f32 or
f64 first), keeps subnormals, overflows to infinity, and keeps a NaN as a quiet NaN with its sign
and the payload's top bits; widening a NaN sets the quiet bit (Arm's `fcvt`/`bfcvt` and x86's
F16C give the same bits; `velt_common::half`, re-exported as `velt_vir::math::half`, is the
reference). LLVM emits `fcvt` on arm64
and F16C under a `--cpu` with it, and the reference's branchless integer sequences elsewhere
(never compiler-rt's `__extendhfsf2` & co.); Cranelift always emits the sequences. F16C's
`vcvtps2ph` rounds by its immediate (nearest even); on AVX512-FP16 CPUs LLVM picks `vcvtss2sh`,
which rounds by MXCSR: the same bits, since Velt never changes MXCSR from round-to-nearest (code
that does, through a native package, changes these conversions too).
`sqrt_f32(x: f32): f32` is not a conversion: the correctly rounded f32 square root (LLVM's
`llvm.sqrt.f32`, Cranelift's `sqrt`), which lowering emits for `Math.sqrt(x) as f16` (or
`as bf16`) of a value of that format, before the one narrowing (`velt_vir` `lower/half.rs`).
`fma_f64(a: f64, b: f64, c: f64): f64` is `a * b + c` rounded once (IEEE 754's
fusedMultiplyAdd): LLVM's `llvm.fma.f64` and Cranelift's `fma`, one instruction where the target
has FMA (arm64, x86-64 with `--cpu x86-64-v3`, a function gated on `fma-x86`) and C's `fma`
elsewhere. std declares it in std/lang/math/fma.velt, and std's `pow` (std/lang/math/pow.velt)
is built on it.

## SIMD

A SIMD value (HIR `TyKind::Simd`, the twelve vector and four mask types) is a VIR `V128`: 16 bytes,
8-byte aligned in memory (loads and stores are unaligned), passed and returned by value between
Velt functions. Lanes are in memory order: lane 0 is the lowest-addressed bytes, as in an array.
A mask lane is all ones or all zeros.

The operations are externs `velt_simd_<type>_<op>` (`velt_simd_u8x16_add`,
`velt_simd_mask8x16_any`) declared by `std/simd/*.velt` (`velt/simd`). **No runtime defines
them**: `velt_vir::simd` gives each symbol's operation and VIR signature (`SimdCall`), the
verifier rejects a `velt_simd_*` extern that is not one, and both backends emit every call
inline. They never cross the C ABI, so no target's vector calling convention is involved.
A 256-bit vector (`f32x8`) is not a VIR type: it is a value of two `V128` halves,
the low lanes first (32 bytes, 8-byte aligned as a `V128` is; the helpers' loads and stores are
unaligned), and `velt_simd_f32x8_<op>` (`SimdCall::Wide`, `velt_vir::simd::wide`)
takes it by address: each vector operand of the 128-bit operation is a `Ptr` to such a value, and a
vector result is written through one more `Ptr`, the last parameter (`add(a, b, out)`,
`splat(x, out)`, `load(array, at, out)`, `store(v, array, at)`). The operation reads the 32 bytes at
each operand pointer, writes the 32 bytes at the result pointer, and does nothing else: it keeps no
pointer, never unwinds and touches no other memory (but for `load` and `store`, their array).
Lowering gives each result a slot of its own; backends read every operand before they write, so a
result pointer equal to an operand's is harmless. Each lane is the 128-bit operation's lane of the
matching half on every target. LLVM computes one vector of twice the lanes (a `ymm` instruction
with AVX; two halves where legalization splits it), Cranelift the two halves in turn, both inline.
To the optimizer a wide operation is a call that writes memory (`math::effects` is `Any`). A local
of this shape is never split into field locals (`velt_opt::sroa`) and is written whole (`dce`'s
zero initialization, the LLVM backend's aggregate assignment), so that LLVM keeps it one value.
The operations are `splat`, `load`, `store`, `add`, `sub`, `mul`, `div`, `min`, `max`, `neg`,
`abs`, `sqrt`, `mul_add` and `mul_add_fast`; the rest of the type is std code on the halves.

`fits_both(a, b, at)` is `fits(a, at) && fits(b, at)` as one test against the smaller
length; no std file declares it: `velt_opt` writes it where the two tests have one lane type and
one index (`fits_pair`). It reads both headers whatever the first length is, so neither array may
be null: where the second can be (an array field read during construction), `fits_pair` tests it
in front of the call, and a null gives false, since the two tests never read it where the first
fails.
`load` and `store` take the array (a pointer to its header) and an element index, and do not
check it: the prelude's methods do. `load_partial` and `store_partial` take a count of elements
too, below the lane count, and touch only those elements. `load_optimistic` is `load`
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

`velt_simd_prefetch(void* addr)` (`-> void`) starts loading the cache line at `addr`
for a read soon, as a hint: LLVM emits `llvm.prefetch` (read, every cache level, data), which
never faults; Cranelift emits nothing. Std reaches it through `arrayPrefetch(xs, i)`
(HIR `Intrinsic::ArrayPrefetch`, the address of element `i`, not bounds-checked).

A shuffle is `velt_simd_<type>_shuffle_<i0>_<i1>…`, one lane index per lane spelled in the symbol
(`velt_simd_u32x4_shuffle_1_0_7_6`; indices below twice the lane count, a lane of the first
argument then of the second), so the mask is a constant wherever the call is: `(a, b) -> v`. No
std file declares one: sema checks the literals of `T.shuffle(a, b, …)` and lowering declares the
extern (HIR `Intrinsic::SimdShuffle`). `velt_simd_<type>_reverse_bytes` (16-, 32- and 64-bit
integer lanes) reverses the bytes within each lane.

`velt_simd_f32x4_mul_add` and `velt_simd_f64x2_mul_add` compute `a * b + c` with one
rounding on every target: the FMA instruction where the CPU has it, and otherwise libm's `fmaf` /
`fma` per lane (which the link provides: `-lm`, the C runtime, compiler-builtins on wasm).
`_mul_add_fast` may fuse or not, whichever is faster (LLVM's `llvm.fmuladd`; a multiply and an add
in Cranelift).

`velt_simd_<type>_sum` (integer lanes, `(v) -> lane`) adds the lanes in the lane type,
wrapping; `velt_simd_<type>_sum_wide` (8-, 16- and 32-bit integer lanes) adds them
extended to `velt_vir::simd::sum_wide_lane`: 32 bits for 8- and 16-bit lanes, 64 for 32-bit ones,
with the lanes' signedness, so it never overflows. LLVM emits `llvm.vector.reduce.add` (of the
extended vector); Cranelift adds the lanes one by one.

`f16x8` and `bf16x8` are 16-bit lanes (`u16` in VIR, as `f16` and `bf16` are): `splat`,
`load`, `store`, `load_partial`, `store_partial`, `fits`, `lane`, `with_lane`, shuffles and
`as_<type>` (and `velt_simd_u16x8_as_f16x8`, `_as_bf16x8`) move their bits. `_widen_low` and
`_widen_high` give `f32x4`, exactly, a NaN quieted. `velt_simd_f32x4_narrow_f16x8` and
`_narrow_bf16x8` `(a, b)` give the lanes of `a` then `b` rounded once to nearest even.
`velt_simd_f32x4_dot_add_f16x8` and `_dot_add_bf16x8` `(acc, a, b)` give lane `k` as
`fma(a[k + 4], b[k + 4], fma(a[k], b[k], acc[k]))`: `llvm.aarch64.neon.fmlal` then `fmlal2` for f16
where the CPU has FEAT_FHM (Apple arm64 with no `--cpu` or `baseline`, or a `--cpu` for which clang
defines `__ARM_FEATURE_FP16_FML`), and otherwise both operands widened and `llvm.fma` twice. A lane
is the same bits on every target unless it is a NaN from more than one NaN operand or from an
invalid operation, where the NaN depends on the CPU and the path (`fmlal` lets a signalling f16 NaN
win over a NaN accumulator, widening first does not), or a signalling NaN, which may or may not
come out quiet (LLVM folds `llvm.fma(1, -0, x)` to `x`, as in `f32x4.mulAdd`).
`velt_simd_f32x4_dot_add_fast_bf16x8` is
`llvm.aarch64.neon.bfdot` where clang defines `__ARM_FEATURE_BF16_VECTOR_ARITHMETIC` for `--cpu`,
`llvm.x86.avx512bf16.dpbf16ps.128` where it defines `__AVX512BF16__` and `__AVX512VL__`, and
otherwise `_dot_add_bf16x8`. The start-up check ("CPU check") does not know FEAT_FHM, FEAT_BF16 or
AVX512-BF16: code built for a `--cpu` that has them stops with an illegal instruction on a CPU that
has not. Cranelift always widens and uses `fma`.

**bf16 dot product at run time.** `velt_simd_f32x4_bfdot` `(acc, a, b) -> acc`
is Arm's `bfdot` itself (`velt_vir::simd::ExtOp::Bfdot`, `llvm.aarch64.neon.bfdot`), an
instruction of the extension `bf16-arm` (FEAT_BF16, LLVM's `+bf16`): a caller runs it only where
the CPU has FEAT_BF16, and on another architecture, on wasm and under Cranelift it gives zeros.
It is checked like any other feature ("Checks"; velt/arch/arm's `Bf16.isSupported()`, which
`bf16x8.hasBf16Arm()` calls): the feature word's bit 39, from `getauxval(AT_HWCAP2)` bit 14 on
Linux and the `hw.optional.arm.FEAT_BF16` sysctl on macOS; `false` on Windows, off arm64 and
under Cranelift.
velt/half's `dotBf16Fast` and `dotBf16FastAt` ask once for a dot product and run a function gated
on `bf16-arm`, whose loops are `velt_simd_f32x4_bfdot`, where the answer is yes, and
`velt_simd_f32x4_dot_add_fast_bf16x8` otherwise, so a build for any arm64 CPU runs `bfdot` where
the CPU has it. `bf16x8.dotAddFast`, a step of a caller's loop, asks nothing and is the latter
alone. In code built for a `--cpu` for which clang defines
`__ARM_FEATURE_BF16_VECTOR_ARITHMETIC`, `velt_simd_has_bf16_arm()` is `1` everywhere and nothing
is asked.

`velt_simd_f16x8_<op>` for `add`, `sub`, `mul`, `div`, `mul_add`, `mul_add_fast`, `min`,
`max`, `neg`, `abs`, `sqrt` and the comparisons `eq`, `ne`, `lt`, `le`, `gt`, `ge` (giving a
`mask16x8`) take `(v)` for `neg`, `abs` and `sqrt`, `(a, b, c)` giving `a * b + c` for `mul_add` and
`mul_add_fast`, and `(a, b)` for the rest, and read the lanes as binary16, rounded to nearest even
with subnormals kept, and `_select` moves their bits. They are the same bits on every target but for
which NaN a NaN is, `_mul_add_fast` excepted. LLVM uses `<8 x half>` where the target has the
instructions (Apple arm64 with no `--cpu` or `baseline`, a `--cpu` for which clang defines
`__ARM_FEATURE_FP16_VECTOR_ARITHMETIC`, or `__AVX512FP16__` with `__AVX512VL__`); elsewhere, and
always under Cranelift, each half is widened, computed in `f32` and narrowed, which rounds the same
(f32 has at least 2p + 2 bits), and `neg` and `abs` change the sign bits. `_mul_add` there
multiplies and adds in `f64` and narrows once, which gives the fused result (the argument is at
`half_arith::fused`; in `f32` it would round twice), with no FMA and no library call;
`_mul_add_fast` there computes in `f32` and narrows. The start-up check does not know these
extensions either. `bf16x8` has none of them.

**CPU extensions.** `velt_simd_u32x4_<op>` for the SHA instructions (`sha1c`, `sha1p`,
`sha1m`, `sha1h`, `sha1su0`, `sha1su1`, `sha256h`, `sha256h2`, `sha256su0`, `sha256su1` on
aarch64; `sha256rnds2`, `sha256msg1`, `sha256msg2`, `sha1rnds4_<0-3>`, `sha1nexte`, `sha1msg1`,
`sha1msg2` on x86-64), and `velt_simd_u64x2_<op>` for the carry-less multiplications
(`pclmulqdq_<imm>` on x86-64, `pmull_<imm>` on aarch64, `imm` 0, 1, 16 or 17 choosing the 64-bit
lanes as PCLMULQDQ's immediate does), `velt_vir::simd::ExtOp`, are the instructions
themselves: a caller runs one only where its check is true (velt/arch's `Sha256.isSupported()`,
`Sha.isSupported()`, `Pclmulqdq.isSupported()`, `Pmull.isSupported()`). On another architecture,
wasm and Cranelift the check is `false` and the operation, dead behind it, gives zeros.

**Checks.** A feature's check is `velt_simd_has_<feature>() || (velt_simd_on_<arch>() &&
(word & mask) == mask)`, one static method per feature in velt/arch/x86 and velt/arch/arm, with
`mask` its bit and those of the features it implies (std/lang/cpu/masks.velt):

- `velt_simd_has_<feature>() -> bool` (`<feature>` the `@targetFeature` name with `-` and `.` as
  `_`: `velt_simd_has_sse4_2_x86`) is `true` only where the build guarantees the feature: in a
  function gated on a set that covers it (and `velt_opt`'s clones for it), or in code built for a
  CPU whose features hold its mask (`--cpu`, "CPU check"). It is `false` everywhere else; the
  runtime keeps no flags.
- `velt_simd_on_x86() -> bool` / `velt_simd_on_arm() -> bool` are `true` in LLVM code of that
  architecture and `false` on the other one, on wasm and under Cranelift (lowering knows the
  `false` ones, so a run-time check folds away there).
- `velt_simd_cpu_word() -> u64` loads the word (`velt_cpu_word` under LLVM, written once by
  `cpuStart` with `velt_simd_cpu_word_set(word)` before any other Velt code and never after, "CPU
  check") as invariant, so the load leaves loops. 0 under Cranelift and on wasm, where the set
  does nothing.

**AES.** `velt_simd_u8x16_<op>` for the AES round instructions (`aesenc`,
`aesenclast`, `aesdec`, `aesdeclast`, `aesimc`, `aeskeygenassist` (immediate 0) on x86-64;
`aese`, `aesd`, `aesmc`, `aesimc_arm` on aarch64), `(state, key) -> state` or `(a) -> a` on the
bytes. Their features `aes-x86` (AES-NI with PCLMULQDQ, SSSE3 and SSE4.1) and `aes-arm`
(FEAT_AES with FEAT_PMULL) are checked like any other ("Checks"). `velt_simd_cpuid(leaf,
subleaf) -> u32x4` is x86's `CPUID` (EAX, EBX, ECX, EDX in lanes 0 to 3), emitted
inline (no runtime function) and pure; it gives zeros on another architecture, on wasm and under
Cranelift. A function gated on `aes-x86` (`aes-arm`) has the carry-less extension of its
architecture too (`Ext::covers`): its `velt_simd_has_clmul_x86` (`_arm`) is `1` as well, and in
any gated function the checks of the other architecture's features are `0`. `--cpu` makes the
x86 check constant where the CPU it names has AES (bit 23).

**FMA.** The extension `fma-x86` (FMA3 with AVX) has no instructions of its
own: a function gated on it compiles `velt_math_fma_f64` (Math operations) to one `vfmadd`,
where the default x86-64 level calls C's `fma`. It is checked like any other feature
("Checks"; velt/arch/x86's `Fma.isSupported()`, which std's `hasFmaX86()` calls): FMA with AVX,
whose bits the feature word sets only where XCR0 says the OS saves the AVX state.
`velt_simd_xgetbv(xcr: u32) -> u64` is x86's `XGETBV`
(EDX:EAX), emitted inline (no runtime function) and pure, valid only where CPUID says OSXSAVE;
it gives 0 on another architecture, on wasm and under Cranelift. With an FMA level (`x86-64-v3`)
the check is constant and `velt_math_fma_f64` is one instruction everywhere.

LLVM gives a function that calls one of these operations the extension's own feature (`+sha2`,
`+sha`, `+pclmul`, `+aes`), which it uses for nothing else; a *gated* function (VIR
`Function::target`, HIR `Program::cpu_targets`, from the decorator `@targetFeature("sha-arm" |
"sha-x86" | "clmul-x86" | "clmul-arm" | "aes-x86" | "aes-arm" | "fma-x86" | "bf16-arm")` in any
module, every call of which sema keeps behind the check) gets the extension's whole set
(`+sha,+ssse3,+sse4.1` on x86), and the check of its own extension (`velt_simd_has_<ext>`) is the
constant `1` there, so the emulation behind it is dead code. The ordinary functions it calls that
check that extension (the `velt/simd` methods), themselves or through their callees, are cloned for
it by `velt_opt` with the check `true`, so this holds whether or not they would be inlined. A gated
function is never inlined into an ordinary one
or one gated on another target (`velt_opt`, and `noinline` on the call for LLVM); ordinary code may
be inlined into a gated function.

## Atomics

The atomic operations ([design/atomics.md](../design/atomics.md)) are externs
`velt_atomic_<op>_<bits>_<order>` on the address of a naturally aligned word, which lowering
calls for `Atomic<T>` (std/prelude/atomic.velt) and for `shared<int>`'s `add`, `get` and `set`
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
LL/SC loops, as in Rust. Cranelift's atomics are all sequentially consistent, which is correct
for every ordering. On wasm without threads they are plain loads and stores.
`velt_rt_thread_yield()` (a real runtime function, rt_abi_async.md §9) is what `velt/sync`'s
`Backoff` (std/sync/backoff.velt) yields with.

## Thread-local values

A module constant `new ThreadLocal<T>(init)` (std/prelude/thread_local.velt; `T` a number type
or `bool`, `init` a literal) is one value per thread. Lowering calls
`velt_tls_get_<id>_<bits>_<init>` (`() -> uN`, this thread's value) and
`velt_tls_set_<id>_<bits>_<init>` (`(uN)`): `id` is the 64-bit FNV-1a hash of
`<qualified name>:<VIR type>` (decimal; `velt_vir::tls::stable_id`, called from
`lower/thread_local.rs`, so it stays the same when other declarations change and changes with
the constant's type), `bits` 8, 16, 32 or 64, and `init` the starting
word in lowercase hex. Values are passed as unsigned integers of the width, as for the atomics (a
float's bits, a `bool` as 0 or 1). **No runtime defines them**: `velt_vir::tls` gives each
symbol's meaning and VIR signature (`TlsCall`), and each backend implements them.

| Backend and target | Storage | A call |
|---|---|---|
| LLVM (every target) | `@velt_tls_data_<id> = hidden thread_local(initialexec) global iN init`, defined by the unit that defines `velt_main` and declared by the others | a load or store through `llvm.threadlocal.address` |
| Cranelift on ELF, Mach-O, x86-64 COFF | a `hidden` thread-local data object `velt_tls_data_<id>` (`tls_model` `elf_gd`, `macho`, `coff`), which LLVM-built objects of the std cache use too | a load or store through `tls_value` |
| Cranelift JIT (`velt dev`), arm64 COFF | an operating system thread key, made the first time it is used, in `velt_tls_key_<id>` (the key plus one): defined by the object, or, in `velt dev`, a slot the session owns and every version imports, so all versions use one key | a call of the extern, which the module defines on `pthread_getspecific`/`pthread_setspecific` (`TlsGetValue`/`TlsSetValue` on Windows); the key holds the word XOR `init`, so a thread that never set it reads `init`. A failed key creation traps rather than use another key |

LLVM strips thread-local globals to plain ones for single-threaded WebAssembly.

## Module state

```c
struct ModuleDesc {
  uint64_t size, align;
  void (*init)(void *state);
  void (*drop)(void *state);
  const char *names;   /* each slot's name, NUL-terminated, in slot order */
  void (*rest)(void *state); /* the root file's slots, or NULL */
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
synchronous `main`) until `main`'s root starts (std's `schedBlockOn`, `velt_rt_block_on` on
WebAssembly) or until `main` returns. A program that never
calls it pays nothing per task (a process-wide flag the first call sets). `velt_rt_mutex_lock`
keeps the module lock while it blocks; waiting for the module lock while the thread holds a
`Mutex` stops with an `ICE:` panic (sema rejects module state in a `with` callback). On
WebAssembly (one thread) there is no lock.

`uint8_t velt_rt_module_with(void)` and `void velt_rt_module_with_end(uint8_t took)`: the first
takes the module lock unless the thread holds it and answers whether it took it (1); the second,
called after the `Mutex` or `RwLock` unlock with that answer, gives it back (unless the callback
reached module state: the poll, or the code outside a task, keeps it then). A program that gives
functions properties through `any` calls them around each `with`, `read` and `write` whose callback
may reach a declared function's properties (below), which are module state: every callback sema does
not list in `Program::lock_free_callbacks` (hir_encodings.md "Module state"). Such a callback holds
the module lock already, so no thread waits for it while holding a `Mutex`; the other callbacks
never reach module state, so a thread holding the module lock that waits for their `Mutex` only
waits for them to finish.

Within one call of a compiled function (until the poll ends, or until `main`'s root starts or
`main` returns), a successful `velt_rt_module_state(desc, s)` stays valid for every slot `<= s`:
the same block, still initialized up to `s`, with the lock still held. The compiler reuses it
instead of calling again (`velt_opt` `module_state`), so a runtime change must keep this.

## Process statics

```c
typedef struct {
    uint64_t size;        // bytes of the table; the runtime reads only the slots it covers
    uint64_t drop;        // address of: void (*)(void), releasing the static atomics' references; or 0
    uint64_t str_epoch;   // address of: _Atomic uint64_t, std's STR_EPOCH; or 0
} VeltStatics;            // later slots are appended: an older table has a smaller size
void velt_rt_statics(const VeltStatics *t);
```

Each address takes an 8-byte slot on every target: on wasm32 it is the slot's first half, the
second half zero (as in the symbol registry's table), so the layout is the same everywhere
(`velt_rt` `statics.rs` asserts the offsets).

`velt_main` calls it first thing, before anything can free a string, with a read-only table in
the program's data. No call when every field would be NULL.

- `drop`: in a program with static atomics holding a reference (a module `const X = new
  Atomic<shared<T> | null>(null)`, hir_encodings.md "Static atomics"), a generated function that
  releases the reference each such word holds and stores null into it. The runtime calls it once
  at exit, after dropping the module state, and only when it drops the module state (the leak
  counters of the debug runtime, `VELT_RC_STATS=1`; § Module state). The WebAssembly runtime
  ignores it.
- `str_epoch`: std's process static `STR_EPOCH` (velt_common `std_hooks.rs` `str_epoch`, a
  `process_static` entry; std/lang/hooks/process_statics.velt), the epoch of the per-thread
  position cache (`velt_rt` `str/recent.rs`). The runtime raises it above every epoch it gave out
  before and uses it from then on: it bumps the word when a buffer the cache may remember is freed
  or moved. The word is Velt's, so the runtime needs no symbol of the program (the shared runtime
  of debug builds is linked before the program exists).

## Breadcrumbs

The position translation of long non-ASCII heap strings (UTF-16 units ↔ WTF-8 bytes) is done by
the runtime (`velt_rt` `str/crumbs.rs`, `str/recent.rs`) and by std (std/lang/string/
`positions.velt`, `crumbs.velt`), which read, build and extend the same tables. Both follow this,
byte for byte:

- **Header field.** A non-ASCII heap buffer is `[crumbs][lone: u64][count: u64][text]`: `crumbs`,
  an 8-byte word 24 bytes before the text on every target (wasm32 too: std's `usize` is 64-bit
  there), is 0 (none), 1 (no table, but a thread remembers a position in a slice of the buffer, so
  freeing it bumps the epoch) or a table's address.
- **Table.** `[len: u64][cap: u64][prev: u64][entries: u32 × cap]` on every target (`prev` an
  address, or 0), allocated with `velt_rt_alloc(size, 8)` (std's `rawAlloc`) and freed with
  `velt_rt_free`. Entry `k` is the byte offset of unit `k × 64`, with bit 31 set when that unit is
  the second half of the 4-byte sequence there; entry 0 is 0.
- **Publishing.** Entries below `len` never change. A writer stores an entry (relaxed) before `len`
  covers it, raising `len` with an atomic `max` (release); a reader loads `len` (acquire) first. A
  table with room is extended in place (two writers store the same values); one without is
  replaced by a copy twice its size (or as large as needed), whose `prev` is the table it
  replaces, published by a compare-and-swap of the header field (acquire-release; acquire on
  failure). A copy that loses is freed at once and the winner's table extended. The runtime frees
  the chain with the buffer.
- **Epoch.** A thread's cache of its last translations (per side: the runtime's in a Rust
  thread-local, std's in `ThreadLocal` words) holds an entry of a heap string under std's
  `STR_EPOCH` (§ Process statics), which the runtime bumps before a buffer with a table or with
  `crumbs` 1 is freed or moved. Static strings' entries never expire.

## Array length limit

An array (`{ data, len, cap }`) holds at most `MAX_ARRAY_LEN` = 2^52 - 1 elements, and its buffer at
most 2^52 - 1 bytes (`cap * size(T)`), so a byte size never wraps. Generated code checks it where it
chooses a length (`velt_vir` `lower/array`: `with_capacity`, `reserve`, an array adopted from native
code, and the growth of an array of zero-sized elements); `velt_rt_realloc`, which generated code
calls only to grow an array, refuses a `new_size` past it (`velt_rt_alloc` does not check: it is on
every `new`); the runtime checks the length in `VeltBytes::from_vec`, `VeltArray::from_vec` and
`VeltStrArray::from_vec` before handing a Rust `Vec` over. Past it, both stop with `panic:
RangeError: Invalid array length: an array holds at most 2^52 - 1 elements, and its elements at most
2^52 - 1 bytes` (exit code 101). Optimizations may rely on it: `velt_opt`'s `numrep` takes an
array's `len` to be at most 2^53 - 3 (all an index running to `len` inclusive needs). A typed
array's byte length is below 2^52 for the same reason (its buffer is a `u8[]`).
`velt_rt_bytes_zeroed(n)` and `velt_rt_bytes_uninit(n)` check `n` against the limit too. A buffer
within the limit that the target cannot allocate is out of memory (`velt_rt_alloc` above), never a
smaller block: on wasm32 a byte size past 2^31 - 1 stops the program there, in `velt_rt_realloc`, in
`velt_rt_bytes_zeroed` and in `velt_rt_bytes_uninit` (which `u8[]`'s `concat` allocates with).

`cap` read as an `i64` says whose the buffer is: positive, a capacity in elements of a buffer the
array owns; 0, no buffer or memory the array does not own (a constant table, std's `addressArray`);
negative, memory with an owner, whose block's address is `cap` without bit 63 (std's
`addressArrayOwned`: dropping the array calls the function in the block's first word with the
block). An array of the last two kinds is never passed to the runtime by value: the runtime's
`cap > 0` tests (`velt_rt_bytes_drop`, `VeltBytes::take_vec`, `velt_rt_str_array_drop`) are
unsigned and would free it.

## Array methods on a proxy

```c
int64_t velt_rt_proxy_array_call(const void *target, int64_t op);
int64_t velt_rt_proxy_array_hole(const void *array, int64_t index, int64_t op);
```

The state of the generic array methods `velt/lang/proxy/array` runs on a proxy of an array
(`p.push(…)`, `p.pop()`, …), per thread, so that the standard library keeps no module state.
Arrays are identified by the data pointer of the `any` holding them. Only a running method's
target is ever recorded: its proxy keeps it alive, so its address is not reused while the
method runs, and the records of a target go when its method returns. std reaches both through
the intrinsics `proxyArrayCall` and `proxyArrayHole` (std/lang/intrinsics/any.velt).

`velt_rt_proxy_array_call(target, op)`: `op` 1 starts a method on a proxy of `target` (methods
nest), -1 ends it and answers the first hole it leaves in `target` (forgotten then; -1 for
none; the standard library makes a left hole a `TypeError`), 2 starts one of the method's own
trap calls (`has`, `get`, `set`, `deleteProperty` on the proxy), -2 ends it. Between trap calls
(the method's user callbacks) no trap call is running.

`velt_rt_proxy_array_hole(array, index, op)`: Node's empty slots, which Velt's arrays don't have.
`op` 0 answers whether element `index` is one (1 or 0); 1 records it, only during a trap call of a
method on `array` (answering whether it was one), else answers -1 and records nothing; 2 forgets it
(whether it was one); 3 forgets the holes from `index` on (a shorter length; 0); 4 answers the first
hole below `index`, or -1. `Reflect` (std/lang/proxy/reflect.velt) records them (a delete, the gap
before an element written past the end) and reports them as absent; elsewhere both are `TypeError`s.
Both runtimes provide them (`velt_rt/src/proxy_array.rs`, compiled into velt_rt_wasm too).

## Host values

```c
void velt_rt_host_release(uint32_t slot);
uint32_t velt_rt_host_clone(uint32_t slot);
/* WebAssembly (velt_rt_wasm), exported from browser modules for the JS glue: */
const uint8_t *velt_host_str_ptr(const VeltStr *s);
size_t velt_host_str_len(const VeltStr *s);
uint8_t *velt_host_alloc(size_t len);
void velt_host_str_new(VeltStr *out, uint8_t *buf, size_t len);
void velt_host_bytes_new(VeltBytes *out, uint8_t *buf, size_t len);
```

What host functions (https://veltlang.com/docs/reference/host-functions/) need. A `HostRef`
(std/host/host_ref.velt) holds a slot of the JS glue's table of values (`velt_web.mjs`), 0 once
released (and natively); its drop calls `velt_rt_host_release` and `clone()` `velt_rt_host_clone`,
which on `wasm32-unknown-unknown` call the glue's imports `velt.host_release` / `velt.host_clone`.
Natively and under WASI no `HostRef` exists (no host function can be called): velt_rt's versions do
nothing and return 0.

The glue reads a string argument (passed as `const VeltStr *`) through `velt_host_str_ptr` and
`velt_host_str_len`. A string or `u8[]` result goes through a trailing out-pointer: the glue
gets a buffer from `velt_host_alloc`, writes the bytes, and `velt_host_str_new` (UTF-8; it
copies and frees the buffer) or `velt_host_bytes_new` (it adopts the buffer) writes the value.
velt_link exports the five from every browser module.

std declares host functions of its own, whose module the glue provides itself, before (and
overridable by) the embedder's `imports`: `velt:fetch` (std/net/http/fetch/client/transport/
js_host.velt), `fetch`'s transport on JS hosts over the host's `fetch` (`fetchHost` in
velt_web.mjs, which a host such as the Workers glue can call with a `fetch` of its own). A call
is a JS object (a `HostRef`) holding the request's `AbortController`, then its `Response`, the
body's reader and the piece last read; its async functions fulfill with nothing and reject with
an error whose `name` is the `IoError` code native `fetch` gives the failure (`AbortError` once
the call was aborted). `velt:random` (std/lang/random/seed_js.velt): `fill(buf: u8[])`, the seed
of `Math.random()`'s generator, which the glue fills with `crypto.getRandomValues` (`randomHost` in
velt_web.mjs); `buf` crosses as a view of the program's memory. `velt:clock`
(std/lang/hooks/clock_js.velt): `now(): f64` and `date(): f64`, the host's `performance.now()`
and `Date.now()` in milliseconds, which `Date.now()` and `performance.now()` read (`clockHost`
in velt_web.mjs).

## Views

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

Record views (velt_vir `lower/record_view.rs`) use the same object table: the live
`Record<string, any>` view of the object an `any` held, keyed by that object's address with bit 0
set (`obj | 1`), which no `Proxy` entry (keyed by an object's address, always even) can be.
`view` is the record object, and its drop glue (`[Symbol.dispose]` time, before its fields drop)
removes the entry before it releases the source.

The `velt_rt_array_view_*` functions keep a second table the same way for array views (velt_vir
`lower/array_view.rs`, hir_encodings.md "Array views"): from (`arr`, the data pointer of the
`any` holding the original array; `ty`, a number the compiler gives the view type, unique
within the program) to `view`, the view's box. A conversion asks it first and records the view
it makes; the view's drop glue removes the entry, and so does a transfer that turns the view
into a plain array in place. `velt_rt_array_view_remove` takes the view's view object (the
`ArrayView`, untagged) and removes the entry if it is that object's view: if the entry's box
header holds `object | 1`. So the view's own box never reaches the runtime, and the backend
need not assume that a call may change a box of a view type.

A view's header is told apart by bit 0 of its data pointer, so an array the runtime hands to
generated code never has an odd data pointer: an empty one has an even dangling pointer
(`velt_rt::bytes::empty_data`, not Rust's `NonNull::dangling()`, which is 1 for bytes).

`velt_rt_array_view_cold()` does nothing. The test before a loop versioned for a box
(`lower/view_unswitch.rs`) calls it when the box is a view, on the way into the loop's
original, and the LLVM backend declares it `cold` (and touching only memory compiled code
never reads), so that it treats the loop's copy for a plain array as the one that runs.

## Function properties

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
use, which the caller stores into the value as its environment), and `identity_of(code)` the
same without making one (null when there is none). A value converted to `any` gets it there, so
`===`, `Map` keys and properties see one function. An identity's entries change no count (its
drop does nothing, its clone and transfer answer the same environment), so every thread may hold
it, and a value holding one takes it along to another task: the function there is the same one.
Its properties are module state (above), since their values are not atomic: `get`, `set`,
`delete`, `len` and `key` on an identity take the module lock first, as an access of a
module-level `let` does, and the task keeps it in its polls from then on; a program that may
give functions properties calls `velt_rt_module_with` before each `Mutex` and `RwLock` lock
whose callback may reach them, so such a callback holds it already. The `set` on an identity
keeps its entries. The table keeps identities to the end; with the leak counters on
(`VELT_RC_STATS=1`), they are freed at exit with their properties, as module state is.

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

## Output
`stream`: 1 = stdout (buffered, flushed at exit / before any stderr write / on `velt_rt_flush`),
2 = stderr.

| Symbol | Signature |
|---|---|
| `velt_rt_write_str` | `(uint32_t stream, const VeltStr* s)` |
| `velt_rt_write_f64` | `(uint32_t stream, double v)` — `console.log` formatting (`1`, `1.5`, `1e+21`, `NaN`, `-0`); called by std's `numberWrite` for a float that is not a whole number below 2^53 or is `-0`, by `wholeNumberWrite` for 10^8 and up, and by lowering where sema left `numberWrite` unchecked |
| `velt_rt_write_bool` | `(uint32_t stream, uint8_t v)` |
| `velt_rt_write_byte` | `(uint32_t stream, uint8_t b)` — used for `' '` and `'\n'` |
| `velt_rt_flush` | `(void)` |

`console.log(a, b)` lowers to
`write_<a>(1, a); write_byte(1, ' '); write_<b>(1, b); write_byte(1, '\n')`, except that a number
is written by std's hooks (std/lang/hooks/number_text.velt and number_write.velt) in one call
with the byte after it: `intWrite(1, a, ' ')`, `uintWrite` (integers, extended to 64-bit; their
text through `write_str`) and `numberWrite` (floats, `f32` converted to `f64`: a whole number
below 2^53 other than `-0` by `wholeNumberWrite` (through `write_str` below 10^8, `write_f64`
above), anything else through `write_f64`; `velt_opt` numrep calls `intWrite` in its place for a
known whole number where the program has `intWrite`, else `wholeNumberWrite`).

## Memory
| Symbol | Signature |
|---|---|
| `velt_rt_alloc` | `(uint64_t size, uint64_t align) -> void*` — never null for size > 0: a request it cannot satisfy stops the program with `panic: memory allocation of <size> bytes failed` (exit code 101), as does `velt_rt_realloc`. On a 32-bit target (wasm32) a size past `isize::MAX` is such a request: the 64-bit size is never truncated |
| `velt_rt_realloc` | `(void* p, uint64_t old_size, uint64_t align, uint64_t new_size) -> void*` |
| `velt_rt_free` | `(void* p, uint64_t size, uint64_t align)` |

In a release build for a native target, every allocation, growth and free calls mimalloc
directly: the optimizer's last step (`velt_opt` `heap_calls`) turns each call of the three
functions into a call of std's hook for it (std/lang/hooks/heap.velt: `heapAlloc`, `heapRealloc`,
`heapFree`), which calls mimalloc through `declare c` (`mi_malloc`, `mi_malloc_aligned`,
`mi_realloc`, `mi_realloc_aligned`, `mi_free`, in the runtime's archive) with the same contract: a
size of 0 gives `align` back and its free does nothing, an alignment of 8 or less takes the plain
function, and growth past the array length limit or a request mimalloc refuses stops the program
with the runtime's message and exit code (through `velt_rt_write_str` and `velt_rt_exit`, not a
panic a `catchVeltPanic` could catch or that could unwind through a `nounwind` call). Array growth
calls `velt_rt_realloc` for an empty buffer too (an old size of 0 allocates). The runtime's Rust
uses mimalloc too (its global allocator), so blocks move both ways.

An allocation of a constant size and a free call the hook itself, inlined. An allocation of a
run-time size (an array buffer) and growth call a wrapper of the hook instead, `velt.heap.alloc`
and `velt.heap.realloc`: internal functions with `velt_rt_alloc`'s and `velt_rt_realloc`'s
signatures and contracts, whose bodies are the hook. velt_codegen_llvm gives the wrappers'
definitions and declarations the attributes it gives the runtime's two declarations, and keeps them
out of line (`noinline`), so to the loops that fill or grow an array they are the same call (with
the hook inlined there, LLVM compiled those loops worse: `heap_calls` says how). The attributes,
which later passes and backends must keep:

| Function | Result | Parameters | Function attributes |
|---|---|---|---|
| `velt_rt_alloc`, `velt.heap.alloc` | `noalias noundef` | `align`: `allocalign` | `nounwind allockind("alloc,uninitialized,aligned") allocsize(0) memory(inaccessiblemem: readwrite) "alloc-family"="velt_rt_alloc"` |
| `velt_rt_realloc`, `velt.heap.realloc` | `noalias noundef` | `p`: `allocptr`; `align`: `allocalign` | `nounwind allockind("realloc,aligned") allocsize(3) memory(argmem: readwrite, inaccessiblemem: readwrite) "alloc-family"="velt_rt_alloc"` |
| `velt_rt_free` | | `p`: `allocptr` | `nounwind allockind("free") memory(argmem: readwrite, inaccessiblemem: readwrite) "alloc-family"="velt_rt_alloc"` |
| `mi_malloc`, `mi_malloc_aligned` | `noalias` | | `nounwind memory(inaccessiblemem: readwrite)` |
| `mi_realloc`, `mi_realloc_aligned`, `mi_free` | | | `nounwind memory(argmem: readwrite, inaccessiblemem: readwrite)` |

Every parameter of these functions is `noundef`. The `mi_*` declarations have no `allockind`: with
it LLVM deletes an allocation whose block is never read together with the hook's null check after
it, so a request larger than memory would no longer stop the program
(tests/golden/lang/array_from_past_memory.velt). A call of the runtime's functions or of a wrapper
keeps its check inside, where LLVM doesn't see it. Debug builds, the JIT and WebAssembly keep calling the
runtime's functions, and so does a release build that links a debug runtime (a `velt` built by
cargo, or `$VELT_RT_LIB` naming one): the variables that make it check and count every block
(`VELT_RT_DEBUG_ALLOC=1`, `VELT_RC_STATS=1`) are read when the program runs. So does one whose
runtime has no mimalloc (built with `--no-default-features`), or that is made with one of those
variables set. The compiler reads the runtime's symbol index to tell
(`velt_link::runtime_lib_profile`); `VELT_HEAP_HOOKS=0` turns the hooks off, `=1` on with a debug
runtime too (veltc's driver/heap_hooks.rs).

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

## Control
| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_panic` | `(const VeltStr* msg) -> noreturn` | flush stdout, print `panic: <msg>\n` to stderr, exit 101; inside a promise that catches panics, unwind to it instead (`extern "C-unwind"`, rt_abi_async.md §1.5) |
| `velt_rt_exit` | `(int32_t code) -> noreturn` | flush stdout, exit |
| `velt_rt_uncaught_report` | `(void)` | called before an `Uncaught …` report is written (the report then ends the process): returns on the first thread to report (and on it again); any other thread waits until the process ends, so one report is printed |
| `velt_rt_set_throw_loc` | `(const VeltStr* loc)` | per-thread slot: where the error being thrown comes from (static ` at <path>:<line>:<col>` string, or null) |
| `velt_rt_throw_loc` | `(void) -> const VeltStr*` | the slot's value (null if never set); appended to `Uncaught …` reports |
| `velt_rt_pow_i64` | `(int64_t a, int64_t b) -> int64_t` | wrapping; negative exponent → 0 (1 if a == 1) |

Division by zero: lowering emits a check that calls `velt_rt_panic` with message `division by zero`.
With source locations (`lower_with`), compiler-emitted panic messages end in
` at <path>:<line>:<col>` (e.g. `division by zero at examples/foo.velt:12:7`).

## Async, I/O, HTTP, sync helpers
See [rt_abi_async.md](rt_abi_async.md) (state-machine poll/drop protocol, `VeltFut`,
spawn/join, `Promise.all`, sleep/yield, fs/net/http/process, `VeltErr`/`IoResult`, refcounts,
mutex, hashing, the other string and byte-array functions).

## Byte arrays, number formatting, binary stdio
Declared in std (Velt signatures; `u8[]`/`string` are passed as in rt_abi_async.md, handles are
`u64`), checked against the runtime by `crates/velt_rt/tests/std_externs.rs`:
- `u8[]` (`std/prelude/bytes.velt`, `std/lang/bytes/copy.velt`):
  `velt_rt_bytes_zeroed(n: u64): u8[]`,
  `velt_rt_bytes_uninit(n: u64): u8[]` (contents not set: `Buffer.allocUnsafe`),
  `velt_rt_bytes_set(dst: u8[], src: u8[], offset: u64): bool` (false: out of range),
  `velt_rt_bytes_copy(dst: u8[], src: u8[], targetStart: i64, sourceStart: i64,
  sourceEnd: i64): i64` (Node's `src.copy(dst, …)`: the bytes copied, −1 for a position out of
  range), `velt_rt_bytes_copy_within(b: u8[], target: i64, start: i64, end: i64)`,
  `velt_rt_bytes_slice(b: u8[], start: i64, end: i64): u8[]`. Searching (`indexOf`,
  `lastIndexOf`, `includes`), filling and concatenating (`std/lang/bytes/search.velt`,
  `copy.velt`) and XOR masking (`xorMask`) are Velt code on SIMD vectors, and UTF-8 validation
  (`utf8Valid`, `validPrefix`) is in `std/encoding/utf8.velt`.
- On a range `[start, end)` (relative indices) so no slice is made:
  `velt_rt_bytes_range_to_str_lossy(b: u8[], start: i64, end: i64): string` (`u8[]`'s
  `utf8Slice` in `std/prelude/bytes.velt`; an invalid sequence as U+FFFD, one per maximal
  invalid subpart): making a string is the runtime's. The runtime also exports
  `velt_rt_bytes_range_to_str(b: u8[], start: i64, end: i64): IoResult<string>`
  (`INVALID_DATA` if not UTF-8), which no std file declares: the WebSocket reader
  (`std/net/websocket/text.velt`) validates with `validPrefix` and then calls `utf8Slice`.
  Writing a string's bytes and comparing them (`write`, `utf8Equals`) is Velt code that reads the
  string's bytes in place (intrinsics `StrByteAt`, `StrLoadU8x16`, `StrLoadPartialU8x16`).
  After `const n = strByteLen(s)` with a local `s`, in a function that is not async, a generator,
  generic or a constructor, `StrByteAt` of `s` in the rest of that block reads through a pointer
  worked out once, right after that statement, when the rest of the block reads a byte of `s` and
  is quiet (`lower/quiet.rs`, strict): nothing there can grow a builder or replace what `s` borrows
  (`lower/str_data.rs`).
- Numbers (`toFixed`, `toExponential`, `toPrecision` and `toString(radix)` are std's:
  velt/lang/number, velt/lang/hooks/number_digits):
  `velt_rt_math_umulh(a: u64, b: u64): u64` (high 64 bits of the product),
  `velt_rt_math_f64_bits(x: f64): u64` and `velt_rt_math_bits_f64(b: u64): f64` (IEEE 754 bit
  casts, `x.toBits()` / `f64.fromBits(b)`; both backends emit them inline).
- Binary stdio: `velt_rt_stdout_write_bytes(bytes: u8[])` (synchronous, ordered with
  `console.log`). Standard input is std code (rt_abi_async.md §14.4).
