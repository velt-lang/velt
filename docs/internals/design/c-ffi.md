# Design: native code through `declare c` and `unsafe`

Status: **accepted** (design review on #2187, round 2, 2026-11-03), revision 6: rebuilt around
the owner's decisions of 2026-11-03 (`unsafe` blocks and functions; declarations checked against
C headers; one way to call native code, Rust included; `declare system` folded in;
`NativeHandle`; `Completion<R>`; Velt's reactor in `velt.h` for Rust crates). Area 12 of #1866; also what GUI bindings, llama.cpp, BLAS and
the database drivers need.

**One way to call native code.** `declare c` inside `unsafe` is the only mechanism, whatever
language the library is written in: the platform's C library, bundled C, C++ or Zig sources, a
Rust crate, or a system library. The parts:

1. **`declare c("lib")` declarations**, checked against the library's C headers per target. The
   manifest names each library's source per target. libc is the library `c`; `declare system`
   goes.
2. **`unsafe { … }` blocks and `unsafe function`s** mark every call into native code and every
   raw operation, each with a `// SAFETY:` comment. Only packages the root lists in
   `allowNative` (and std) may contain them, and raw pointers never reach user code.
3. **`NativeHandle<T>`**: a native object's pointer with its release, counting calls in flight,
   so release can't run under a call (.NET's `SafeHandle`).
4. **`PinnedBuffer<T>`**: memory lent to native code past one call, with leases the ownership
   checker treats like borrows.
5. **`NativeCallback<F>`**: Velt functions handed to C, which run through the executor.
6. **Async**: `declare c async` runs a blocking call on the library's pool; `Completion<R>` lets
   native code complete a promise later from any thread, without holding one.
7. **Per-platform backends** as conditional imports: Accelerate, OpenBLAS or MKL behind one
   package.

## Problem

Velt reaches native code today in two ways, neither general:

- **A package's Rust crate** (`native` in `package.vlt`, native_abi.md), with a Velt-specific
  ABI: `VeltStr`/`VeltArray`/`IoResult` out-pointers, handle tables, `velt_p__` exports and
  their signature records, a function table into the runtime. The glue is code on the hot path
  that the compiler can't see: `packages/sqlite` ships rows and parameters through cursors,
  sinks, an `Arc`, a `Mutex` and (in places) JSON, and `bench/db/RESULTS.md` measured
  0.29–0.91× of rusqlite, which makes the same C calls without the layers. Every author needs
  cargo, and what crosses is what the ABI knows: no callbacks, no pointers C keeps, no structs
  by value.
- **`declare system function`** (#1716) binds libc through a signature table inside the
  compiler: safe, but closed to every other library.

A third-party C library has no table in the compiler: the declaration itself is the statement
of the C signature, and a wrong one corrupts memory. So the design has three parts: **what
crosses** (types and lowering), **checking the declaration against the library's headers**, and
**marking and confining the code that relies on it** (`unsafe`, `allowNative`).

## Example: SQLite

```ts planned
import { CFunction, CString, NativeHandle, Out, Ptr, StrBytes, byteLength, readUtf8 } from "velt/ffi";

declare c type sqlite3;
declare c type sqlite3_stmt;
type Destructor = (p: Ptr<void>) => void;

declare c("sqlite3") function sqlite3_prepare_v3(db: Ptr<sqlite3>, sql: StrBytes, nByte: i32,
  flags: u32, stmt: Out<Ptr<sqlite3_stmt> | null>, tail: null): i32;
declare c("sqlite3") function sqlite3_bind_text(stmt: Ptr<sqlite3_stmt>, i: i32,
  text: StrBytes, n: i32, destructor: CFunction<Destructor> | null): i32;
declare c("sqlite3") function sqlite3_step(stmt: Ptr<sqlite3_stmt>): i32;
declare c("sqlite3") function sqlite3_column_text(stmt: Ptr<sqlite3_stmt>, col: i32): Ptr<u8> | null;
declare c("sqlite3") function sqlite3_column_bytes(stmt: Ptr<sqlite3_stmt>, col: i32): i32;
declare c("sqlite3") function sqlite3_finalize(stmt: Ptr<sqlite3_stmt>): i32;
declare c const SQLITE_TRANSIENT: CFunction<Destructor>;   // the value comes from sqlite3.h

export class Statement {
  #stmt: NativeHandle<sqlite3_stmt>;

  // ...made from sqlite3_prepare_v3's Out:
  //   new NativeHandle(stmt, p => unsafe { sqlite3_finalize(p) })

  bindText(i: i32, s: string): void {
    // SAFETY: SQLITE_TRANSIENT makes SQLite copy the text before the call returns.
    check(unsafe { sqlite3_bind_text(this.#stmt, i, s, byteLength(s) as i32, SQLITE_TRANSIENT) });
  }

  text(col: i32): string {
    // SAFETY: the column's text is valid until the next step, reset or finalize of the
    // statement, and is copied before this method returns.
    unsafe {
      const p = sqlite3_column_text(this.#stmt, col);
      return p === null ? "" : readUtf8(p, sqlite3_column_bytes(this.#stmt, col) as usize);
    }
  }
}
```

Each call is one direct C call: no export of a Rust crate, no handle table, no lock, and the
string goes without a copy. The `unsafe` blocks are the package's whole trusted base, each with
its reason; the exported class is the safe API.

## Declarations

`declare c("<library>") function name(params): R;` is a declaration kind of its own in the AST,
HIR and VIR (like `declare host("m")`, `declare intrinsic` and `declare generated("m")`). The
keyword is `c` because it names the ABI, which every language here speaks.

- `<library>` is a **logical name** that the manifest maps to a library per target (below);
  `"c"` is the platform's C library. The compiler never names a platform or a library.
- The link symbol is `name`; a `$suffix` is ignored, so one module can declare one symbol with
  several signatures (`objc_msgSend$rect`, `objc_msgSend$void`).
- **`declare c type sqlite3;`** declares a type that exists only behind pointers: `Ptr<sqlite3>`
  and `Ptr<sqlite3_stmt>` are different types, and a misspelled name is an error with a
  did-you-mean. `Ptr<u8>`, `Ptr<f32>` point at scalars; `Ptr<void>` is `void *`.
- **`declare c struct S { … }`**: a C struct (below).
- **`declare c const NAME: T;`**: a constant whose value comes from the headers per target (a
  macro or an enumerator, evaluated by clang), never written by the binding: a wrong flag is as
  unsafe as a wrong signature (`SQLITE_STATIC` for `SQLITE_TRANSIENT` is a use-after-free).
- `declare c("m") async function` returns a `Promise`: a blocking call on the library's pool, or
  a non-blocking one when it has a `Completion<R>` parameter (see "Async").
- Variadic functions are declared with their fixed parameters and `...args: CVarArgs` (scalars
  and `Ptr`s, C's default promotions, the target's variadic convention: Apple arm64 passes them
  on the stack, unlike AAPCS64).
- A declaration is allowed on every target; a reachable call where the manifest maps no library
  for the target is an error naming the target and the caller, so portable code chooses per
  platform with conditional imports.

Declaring is safe; calling needs `unsafe`.

## `unsafe` blocks and functions

The trusted code is marked where a reviewer finds it, as in Rust and C#. A `.vlt` file is Velt,
which already has much that tsc doesn't accept (`declare c`, `f16`, operator methods, `spawn`,
`shared`): the goal is the best language that looks like TypeScript, with no workarounds.

```ts planned
// SAFETY: `p` comes from sqlite3_prepare_v3 and the handle runs this once.
const release = (p: Ptr<sqlite3_stmt>) => unsafe { sqlite3_finalize(p) };   // an unsafe expression

// SAFETY: callers pass a buffer of at least `n` floats.
unsafe function sum(p: Ptr<f32>, n: usize): f32 { … }
```

- **Syntax.** `unsafe { … }` is a block statement, read like `try { … }`. In expression position
  `unsafe { expr }` holds one expression and evaluates to it (`const rc = unsafe {
  sqlite3_step(s) };`, `p => unsafe { sqlite3_finalize(p) }`). `unsafe` is a modifier on
  functions and methods, read like `async` (`unsafe function`, `unsafe async function`,
  `static unsafe m()`), and an `unsafe` function's body is an unsafe block. Unsafe contexts are
  lexical: the functions that are expressions of an unsafe block (arrows, generator function
  expressions, object-literal methods) are unsafe code; nested function declarations and class
  members (in a class expression too) are not, nor are an unsafe function's default parameters.
  Interface members can't be `unsafe`.
  It is a contextual keyword: `unsafe` stays usable as an identifier where neither `{` nor a
  function follows it on the same line.
- **Unsafe functions are not values.** A function type can't say it is unsafe, so an unsafe
  function used as a value (not called) is an error; a safe arrow calling it in an `unsafe`
  expression is the value. `unsafe` on arrow functions waits for unsafe function types, which
  nothing needs yet.
- **What requires `unsafe`** (an error elsewhere, naming the operation, with `unsafe { … }` as
  the fix): calling a `declare c` function; the raw `velt/ffi` operations (`Ptr` reads and writes
  at an offset, `readUtf8`/`readCString`/`readBytes` from a `Ptr`, reading a `CFunction` from C
  memory and calling it, `NativeCallback`'s `.fn`/`.context`, `PinnedBuffer`'s `lend()` and
  `lendOwned()`, `NativeHandle`'s `use()`); calling an `unsafe function`.
- **What stays safe**: making a `CString`, `Out`, `PinnedBuffer` (and its `.view`), a
  `NativeCallback` or a `NativeHandle`'s `dispose()`; passing a `string` as `StrBytes`;
  everything type-checked.
- **`// SAFETY:` comments.** Each unsafe block and unsafe function needs one, directly before
  it, stating why it is sound. Missing, it is a lint error in packages and std (Clippy's
  `undocumented_unsafe_blocks`). This is what makes the audit possible.
- **Where.** `unsafe` is allowed only in packages with `native.c` that the root lists in
  `allowNative`, and in std (the root of trust). Elsewhere it is an error (`unsafe code needs a
  native package`), so user programs never contain it.
- **Listing.** `velt check` lists each package's unsafe blocks and functions (file and line)
  next to its `allowNative` entry, so an audit reads exactly those places.
- **Tooling**: `--ts-compat` reports `unsafe` as Velt-only; the editor highlights it; `velt fmt`
  formats it. It is a language addition: an entry in the reference and a docs-sync issue.

## What crosses

| Velt | C | Notes |
|---|---|---|
| `bool`, `i8`…`i64`, `u8`…`u64`, `isize`, `usize`, `f32`, `f64` | `_Bool`, `int8_t`…, `ssize_t`, `size_t`, `float`, `double` | by value; `bool` is C's `_Bool` only (one byte, 0 or 1), never an `int` flag. C's `long` and `unsigned long` are 64 bits on Linux and macOS but 32 on Windows (LLP64): a declaration using them is right on one or the other until the header check (or a `c_long` type) |
| `Ptr<T>`, `Ptr<T> \| null` | `T *` | an address; `null` is `NULL` (same word) |
| `NativeHandle<T>` (argument for a `Ptr<T>` parameter) | `T *` | holds an in-flight reference for the call (below) |
| `null` (as a parameter type) | any pointer | always `NULL` |
| `readonly u8[]`, `u8[]`, typed arrays, views (#1718) | `const T *` / `T *` | the first element's address, borrowed for the call; the binding passes the length |
| `StrBytes` | `const char *` | a `string`'s UTF-8 bytes, borrowed, no NUL; the binding passes `byteLength(s)` |
| `CString` | `const char *` | an owned NUL-terminated copy (`new CString(s)`, one allocation, reusable) |
| `CStr` (callback parameters only) | `const char *` | copied into a `string` by the trampoline |
| `Out<T>` | `T *` | a stack slot the call writes, copied back |
| a `declare c struct` | by value, or `S *` as `Out<S>` / `Ref<S>` | the target's C layout and calling convention |
| `CFunction<F>` | function pointer | a `NativeCallback`'s `.fn`, or one read from C memory |
| `CallbackContext` | `void *` | a `NativeCallback`'s `.context` |
| `Lease<T>` | `T *` | a `PinnedBuffer`'s lease |
| `Completion<R>` | `VeltCompletion *` | completes the async call's promise (below) |
| `void` | `void` | |

- **No hidden conversions**: a `string` crosses only as a `StrBytes` parameter (the declaration
  says so) or through a `CString` the binding made; no call allocates on the common path.
- **Strings to C without a copy.** Velt strings are stored as UTF-8 (canonical WTF-8, rt_abi.md
  "Strings"), so a `StrBytes` argument is the string's own bytes. A string whose header says it
  may hold a lone surrogate is copied first with U+FFFD for each (the rule of every other
  output); U+FFFD is 3 bytes like the surrogate, so `byteLength(s)` stays exact. Well-formed
  strings never copy. This is what `sqlite3_bind_text` takes and what rusqlite passes from a
  `&str`.
- **Empty arrays** cross as a non-null address aligned for the element type; only `null` is
  `NULL`.
- **Strings from C** are read explicitly (and unsafely): `readUtf8(p, len)`, `readCString(p)`,
  `readBytes(p, len, into)` copy; invalid UTF-8 becomes U+FFFD. C keeps ownership of `p`.

### Errors: `errno` and `GetLastError`

A declaration asks for the error, as .NET's `SetLastError` does per P/Invoke:

```ts planned
declare c("c", { errno: true }) function open(path: CString, flags: i32): i32;
```

Lowering captures `errno` (`*__error()` / `*__errno_location()`) into the thread's error slot right
after a call of such a function, and `GetLastError()` the same way on Windows; no runtime code
between the call and the check can overwrite it. `lastOsError()` reads the slot, so its adjacency is
a compiler guarantee, and it is meaningful only after a call that asked: any other leaves the slot
alone. An async call that asks captures the error on its thread and carries it back with the result.

Off by default, because what a call pays shows: many libraries report failures through results and
never need it, and capture is a call and a store per C call. SQLite's range select of a hundred rows
costs 9% more with it, 1.04× the Rust crate it replaces against 0.94× without. A check of whether the
whole program reads `lastOsError()` was rejected: what a `declare c` call does must not depend on
code elsewhere.

### C structs

```ts planned
declare c struct CGPoint { x: f64; y: f64; }
declare c struct CGRect { origin: CGPoint; size: CGSize; }
declare c("objc") function objc_msgSend$frame(obj: Ptr<NSView>, sel: Ptr<SEL>): CGRect;
```

Fields in C order with C-crossable types (scalars, `Ptr`, other C structs; fixed arrays as tuples
until `T[N]`, #1866 area 4); in Velt a plain value struct, never counted. Passing and returning
by value follows the target's convention (SysV x86-64 INTEGER/SSE eightbytes, AAPCS64
homogeneous float aggregates, Win64 by reference over 8 bytes, `sret`, Apple arm64 variadics).
**One table-driven classifier**, a port of rustc's `rustc_target::abi::call`, serves both
backends, and a **differential ABI test** (abi-cafe style: generated C functions over many
struct shapes, compiled with the system C compiler, called from both backends, every field
compared, every CI target) is step 4's entry criterion.

## Checking declarations against C headers

Wherever headers exist, every declaration is checked against them, so a wrong `int` for
`size_t` or a wrong struct is a build error, not memory corruption.

```ts planned
native: { c: {
  sqlite3: { sources: ["native/sqlite3.c"], headers: ["native/sqlite3.h"] },
  cblas: { system: { darwin: "framework:Accelerate", linux: "openblas" },
           headers: { darwin: ["Accelerate/Accelerate.h"], linux: ["cblas.h"] } },
} }
```

- **When**: `velt native build` (every publish), CI, and a package author's local build parse
  the headers with the pinned `zig cc -Xclang -ast-dump=json`, **once per target**, since types
  differ by target (`long` is 32 bits on Win64). System headers come from zig's libc headers or
  a pinned SDK.
- **Functions** must exist in the headers. Parameters and results compare by the C type each
  Velt type maps to, after typedefs resolve:

  | Velt | Matches |
  |---|---|
  | `i8`…`u64`, `f32`, `f64` | the C type of that width and signedness; a C enum by its underlying type |
  | `bool` | `_Bool` / `bool` only |
  | `isize`, `usize` | `ssize_t`, `ptrdiff_t`, `intptr_t` / `size_t`, `uintptr_t`, which require them (never `i64`/`u64`), so a declaration right on one target is right on all |
  | `Ptr<T>`, `Out<T>`, `Lease<T>` | `T *`, `T` the C type of that name |
  | read-only arrays, `StrBytes`, `CString`, read-only leases | `const T *` (a non-const `T *` is accepted: many APIs omit `const`) |
  | written arrays, `Out<T>`, mutable leases | non-const `T *` only |
  | `CFunction<F>` | a function pointer matching `F`, compared recursively |
  | `CallbackContext`, `Ptr<void>`, `null` | `void *` (`null`: any pointer) |

  Variadics match exactly.
- **Structs**: names, order, types, size, alignment and offsets per target. **Constants**: their
  values, recorded per target.
- **A mismatch is a build error** naming the declaration and the header line:
  `\`sqlite3_bind_text\` parameter 4 is \`int\` in sqlite3.h:4581; \`n: i64\` declares
  \`int64_t\`: use \`i32\``.
- **Rust crates** are checked against a header the build generates from the crate's
  `extern "C"` exports (cbindgen-style, or from the SDK's derive); Zig against `zig build`'s
  emitted header. libc is checked against each CI target's libc headers in Velt's CI (std's
  declarations), replacing the compiler's table.
- **Installs check what was checked**: `c-headers.json`, at the package root, records per target
  and library a digest of each checked declaration and of each of the package's own headers the
  check read, and the compiler compares them with what it compiles and the headers it has. Users
  never run clang. (A package with only C libraries has no bundle, so the record
  is not in a bundle's `native.json`.)
- **No headers** (a binary-only SDK): `unchecked: true` on the library's entry, shown in
  `allowNative`'s error and `velt check`.

What stays trusted is what no header expresses, and it lives in the `unsafe` blocks with their
stated reasons: lease lifetimes, what a returned pointer points to and how long it lives, raw
reads.

## Native objects: `NativeHandle<T>`

A class that holds a bare `Ptr` and frees it in `[Symbol.dispose]()` lets `dispose()` run while
an async call on that pointer is still on a pool thread: a use-after-free in the binding, the
hazard leases settle for buffers. `NativeHandle<T>` (velt/ffi, after .NET's `SafeHandle`) holds
the pointer and its release:

- `new NativeHandle(ptr, release)` takes ownership; `release` runs **exactly once**.
- **It counts calls in flight** (an atomic count). Passing a handle where a `declare c` parameter
  takes `Ptr<T>` takes a reference for the call: a synchronous call holds it for its duration,
  an async call **moves it into the call** and the pool thread (or the completion) releases it
  when C is done, whatever happens to the awaiting task, as for leases. `handle.use(p => …)`
  (unsafe) lends the raw pointer to a synchronous closure under a reference, for raw reads.
- `dispose()` marks the handle closed; the release runs when the last in-flight call ends.
- Using a closed handle throws `HandleClosed`, a catchable error, never a use-after-free.

The bindings of steps 3 and 6 (SQLite, llama.cpp) hold their handles in it, not in bare `Ptr`
fields.

## Callbacks: `NativeCallback<F>`

```ts planned
type LogFn = (level: i32, text: CStr, ctx: CallbackContext) => void;
declare c("llama") function llama_log_set(cb: CFunction<LogFn> | null, ctx: CallbackContext | null): void;

export function onLog(handler: (level: i32, text: string) => void): Disposable {
  const cb = new NativeCallback<LogFn>((level, text) => handler(level, text),
    { queue: { capacity: 256, whenFull: "drop" } });
  // SAFETY: llama keeps the pointers until llama_log_set is called again; `cb` outlives that
  // because the returned disposable unregisters it first.
  unsafe { llama_log_set(cb.fn, cb.context); }
  return cb;
}
```

`new NativeCallback<F>(closure, options?)` owns a closure and gives `.fn`, a trampoline the
compiler emits for `F`, and `.context`, the user data C passes back; `F` names that parameter
with the type `CallbackContext`, and the closure sees the others. A call after dispose stops the
program with a message (the context is a key with a generation). APIs without user data
(`atexit`, signal handlers) use `NativeCallback.static(f)` with a module-level function.

- **Captures follow `spawn`'s transfer rules** (moved in, or `shared`), so a callback can never
  reach what the caller lent to the C call it runs inside. Rules keyed to a call's parameters
  can't be complete: `gtk_main` takes no function but runs every registered handler. Results
  are collected through a `Channel` (a handle copies share), a read-only table goes in as
  `shared`, and hot paths step and read directly instead of collecting through callbacks.
- **Through the executor, keyed by task.** A call into C records "task T is inside a C call on
  this thread" (two stores); the trampoline reads it (one load). **At once** when T is the
  callback's owner: a nested turn of the parked task (`qsort`, `sqlite3_exec`, a GTK signal
  under `gtk_main`), and the result goes back to C. **Queued** otherwise (C's own threads, or
  after the call returned): the arguments go into the callback's ring and the owner runs the
  closure when next polled, wherever it runs. Only an `F` returning `void` may be queued (else
  the program stops with a message); arguments are scalars, `Ptr`s and copied `CStr`s. A live
  callback holds a keep-alive reference.
- **Bounded rings.** Each callback gets a fixed ring at construction; the trampoline uses
  atomics only (no allocation, no lock: real-time audio threads can call it). Options:
  `capacity` (default 1024), `bytes` for copied strings (64 KiB), `whenFull`: `"drop"` (default,
  counted in `cb.dropped`, with a one-time stderr warning on the first drop), `"overwrite"` or
  `"latest"`. Completions are async calls or `Completion<R>`, never droppable messages.
- A result-returning callback called on its owner's thread with no C call active stops the
  program, until the main-thread loop (step 9) may relax it.
- Errors and panics never unwind into C: an error escaping a callback is a panic, which aborts
  in a trampoline. Hot reload goes through a slot per trampoline, so stored pointers stay valid.

## Buffers C keeps: `PinnedBuffer<T>` and leases

```ts planned
declare c("llama") async function llama_decode_into(
  ctx: Ptr<llama_context>, batch: Ptr<llama_batch>, logits: Lease<f32>): i32;

const logits = new PinnedBuffer<f32>(vocab);
// SAFETY: llama writes `logits` only during this call; the lease is released when it returns.
await unsafe { llama_decode_into(this.#ctx, batch, logits.lend()) };
const top = argmax(logits.view);
```

A pinned buffer is a block outside the counted heap with its own atomic count; it never moves or
grows. Elements are numeric or C structs. Velt reads and writes it through `.view`; native code
gets it only through a lease:

| Lease | Checked | Enforced by |
|---|---|---|
| for a synchronous call (`lend()`, `lendReadonly()`) | compile time | the ownership checker: a mutable or shared borrow of the buffer |
| moved into an async call | run time, only after a cancellation | the lease's count keeps the memory; `.view` stops the program while the block is lent; `await buf.returned()` waits |
| owned (`lendOwned(buf)`), e.g. in a field | compile time | the buffer moves into the lease; `end()` gives it back |

An async lease is released by the pool thread (or the completion) when C is done, whatever
happened to the task; a test cancels during a long call and checks under the debug allocator
that the buffer outlives it. Ending a synchronous or owned lease no earlier than C's last use is
the statement its `unsafe` block's `SAFETY` comment makes.

## Async

Two forms, both `declare c async`, both cancellation-safe by one rule: **what C keeps is moved
into the call** (`CString`, leases, handles' references, the completion), and released when C
is done, whatever happens to the awaiting task. Arguments are scalars, `Ptr`s and moved values;
no borrowed array or `StrBytes` (`StrBytes is borrowed for a synchronous call; an async call
takes a CString`).

**Blocking calls on the library's pool** (`llama_decode`, hundreds of milliseconds). Each library
has its own pool, not the runtime's blocking pool, so file I/O never starves: `threads` in the
package's manifest (default the core count), overridden by the root (`nativeThreads: { llama: 8
}`) and at run time (`VELT_NATIVE_THREADS_LLAMA`). Calls past the limit wait in order; no thread
is spawned past it; a cancelled waiting call leaves the queue and releases what it holds.

**Non-blocking calls with `Completion<R>`** (network drivers):

```ts planned
declare c("pg") async function pg_query(conn: Ptr<pg_conn>, sql: CString, done: Completion<PgRows>): void;
// Velt sees it as: (conn, sql) => Promise<PgRows>
```

The C function takes the completion and returns at once; native code completes it later, from
any thread, through the small public `velt.h`:

```c
void velt_complete(VeltCompletion *done, const void *result);  // R, size and layout from the declaration
void velt_fail(VeltCompletion *done, const char *code, const uint8_t *msg, size_t len);  // IoError
bool velt_cancelled(const VeltCompletion *done);               // the awaiting task went away
void *velt_alloc(size_t size, size_t align);                   // for owned results (u8[], string)
```

`R` is a C-crossable value, or an owned `u8[]`/`string` whose buffer native code made with
`velt_alloc` (adopted without a copy; a `string` is UTF-8-checked). Each completion is completed
or failed exactly once (twice is a fatal error, never a use of freed memory); a cancelled task's
late completion is dropped, freeing an owned result; `velt_cancelled` lets native code stop
early. It is today's `VeltFut`/`Completer` made a public C API.

## Write effects (#854, #2180)

A `declare c` function's write-effect summary comes from its declaration: scalars, read-only
arrays, `StrBytes`, `CString`, read-only leases and handles write nothing (a handle's object is
native memory); written arrays, `Out`, `Ref` and mutable leases write their argument. Callbacks
add nothing (their captures are their own), so a C call is never an unknown callee.

## The trust boundary

- **`allowNative`.** Installing or building with a package that has native code is an error
  until the root's `package.vlt` lists it (`allowNative: ["sqlite", "llama"]`), recorded in
  `velt.lock.json`. The error names the dependency chain and prints the exact line to add. It is
  an error from the start (owner: no outside users yet); the PR that adds it updates the
  templates, examples and playground so `main` stays green.
  An entry binds to a source (#2367): a bare name allows the registry and local paths, a
  `{ name, git }` entry that repository at any commit, a `{ name, url }` entry archives under an
  https prefix. A git repository or an archive names its package whatever it likes, so a bare
  `"sqlite"` never allows a `sqlite` from git; the check runs after resolution, on the source
  that won (the root's own dependency entry wins over transitive ones). Contract:
  [manifest.md](../contracts/manifest.md) "One source per package".
- **`unsafe`** only in `allowNative` packages and std (above).
- **Raw types and unsafe functions stay among native packages.** `Ptr`, `CFunction`,
  `CallbackContext`, `Lease`, `declare c type`s and `unsafe function`s can be imported only by
  `allowNative` packages: a binding may build on a lower-level binding, and user code never sees
  them. Exported classes hold them privately (`#stmt: NativeHandle<sqlite3_stmt>`).
- **`velt check`** lists, per package: its libraries (and `unchecked` ones), and its unsafe blocks
  and functions by file and line (#1773).

## Where libraries come from

Every library is a logical name in `native.c`, with one source per target:

```ts planned
native: {
  c: {
    sqlite3: { sources: ["native/sqlite3.c"], headers: ["native/sqlite3.h"],
               defines: { SQLITE_THREADSAFE: "2" } },
    pgwire: { crate: "native/" },                                  // Rust, built with cargo
    cblas: { system: { darwin: "framework:Accelerate", linux: "openblas", win32: "mkl_rt" } },
  },
  targets: [ /* as today */ ],
}
```

- **`sources`** (C, C++, Zig; C for now, "As built"): built by `velt native build` with the
  pinned `zig cc`/`zig c++`,
  which cross-compiles every target from one machine, reproducibly (fixed flags, paths and
  timestamps), so the registry or any CI can rebuild a bundle and check its checksum. Apple
  frameworks are the exception: linking them needs the macOS SDK, which can't be redistributed,
  so their macOS bundles are built on a macOS host or with an SDK the publisher supplies
  (`--macos-sdk <path>`).
- **`crate`** (Rust): built with cargo per target; exports `#[no_mangle] pub extern "C"`
  functions, declared with `declare c` like any library.
- **`system`**: the platform's library (`-lsqlite3`), a framework, or an import library, keyed by
  the build conditions packages already use for conditional imports.
- All of them get the same prebuilt bundles per target, checksums, cache, header check,
  `allowNative` and listing. Users never need a C compiler or cargo. Release builds link
  statically, debug builds the shared library, the dev JIT `dlopen`s it.
- **`c`, the C library**, is predefined: libc, libSystem, or the UCRT on Windows; WASI's libc on
  `wasm32-wasip1` where system functions exist today.

### Fetched archives (#2367)

Sources and packages may come from a release archive at a URL, pinned by its sha256 (#2367
step 2). One extractor, `vpm::extract`, unpacks every such archive (`.tar.gz` or `.zip`, chosen by
the first bytes). It reads the whole tree and checks it before writing anything:

- **Paths:** it refuses absolute paths, `..`, a backslash, NUL and other control characters, and
  what Windows can't hold or reads differently (`:`, `CON`/`NUL`/`COM1`…, a trailing dot or
  space, an 8.3 short name such as `PROGRA~1`). It also refuses paths deeper than 128 directories,
  two paths that differ only by case or Unicode normalisation (NFC, case-folded), an entry written
  twice, a path that is both a file and a directory, and anything under a link.
- **Links:** symbolic links are followed through the archive's other links, as the OS would
  (finding a link by any case or normalisation of its name, as macOS and Windows do), and refused
  when they leave the tree. A hard link becomes a copy of an earlier file.
- **Entry types and formats:** it refuses devices, FIFOs, sparse files, and encrypted, zip64 or
  multi-disk zips. It also refuses a zip whose local header disagrees with its central directory,
  whose entries share data, or that has bytes no entry accounts for. All of this is checked for
  every entry before anything is decompressed.
- **Limits:** unpacked size, entry count and the compression ratio are enforced while
  decompressing. The bytes of all paths, and the bytes looked at while following links, are
  capped too.
- **Writing:** it writes into a staging directory beside the destination, then renames that into
  place. Every directory and file is created new, never through what is already on disk, so a
  name the filesystem folds in a way the clash key doesn't know is an error, not an escape. Modes and timestamps are ignored. On Windows, links are written as files holding their
  target, as git's `core.symlinks=false` does, so `git::tree_checksum` gives one archive one
  checksum on every OS.

The `extract` fuzz target (`fuzz/extract/`) covers the extractor.

### Rust is a source language, not an ABI

`velt_native` (the Rust SDK) shrinks to a convenience crate: `#[repr(C)]` types matching Velt's C
types (`StrBytes`, slices, `Completion`), a derive that writes the C signatures and the header the
check reads, and safe wrappers of `velt.h` (`velt_complete`). The Velt-specific native ABI
(`VeltStr`/`VeltArray`/`IoResult` out-pointers, handle tables, the `velt_p__` prefix and
signature records, the runtime function table) is retired; native_abi.md is replaced by this
design's contract.

### `declare system` becomes `declare c("c")`

libc is a library like any other: std's system functions become `declare c("c")` declarations,
checked against each CI target's libc headers instead of the compiler's table, which goes
(`velt_vir/src/system/`). The guards the table enforced (`fcntl`'s commands, `CreateFileW` without
`FILE_FLAG_OVERLAPPED`, paths with exactly one trailing 0, a buffer's length taken from the
array) move into std's Velt wrappers around the `unsafe` calls, where their `SAFETY` comments
state them; `velt/fs`'s public API is unchanged. std may use `unsafe` without `allowNative`.

## Per-platform backends

- **Same C API, different library**: one logical library mapped per target (CBLAS on
  Accelerate, OpenBLAS or MKL), one set of declarations.
- **Different APIs**: conditional imports. Each variant declares its own library's functions and
  exports the same names and signatures (the variant check verifies it):
  `"#backend": { darwin: "./src/backend/accelerate.vlt", linux: "./src/backend/openblas.vlt",
  win32: "./src/backend/mkl.vlt" }`. Only the chosen variant's libraries link.
- Choosing between libraries on one platform needs a user-set condition (Node's `--conditions`),
  a separate change to the build CLI and lock file.

## C++

C++ libraries are reached through their C API (llama.cpp, ONNX Runtime, LLVM-C, wgpu-native all
have one), with the C++ standard library linked per target. A C++-only library gets a small
`extern "C"` wrapper in the package's sources that catches every exception; one escaping into
Velt is undefined behavior. Direct C++ interop (mangled names, classes, templates) is a non-goal.

## Prior art: .NET

C# arrived at nearly the same shape: `unsafe` blocks and methods allowed per project
(`AllowUnsafeBlocks`), `[LibraryImport]` declarations, per-platform library resolution,
`Memory<T>.Pin()` for buffers past a call, `SetLastError`, unmanaged function pointers and
`SafeHandle`, which this design takes as `NativeHandle`. Where .NET is weaker, this design is
stricter: no run-time marshalling layer, borrows checked at compile time instead of `fixed`
scopes trusted by convention, and the consumer's explicit `allowNative`.

## How it compiles

- **syntax/sema**: `unsafe` blocks, functions and their checks; the `SAFETY` lint; the crossing
  types; the trust boundary; `NativeCallback` captures under the transfer rules; effect
  summaries; leases as borrows (synchronous) or moves (async).
- **VIR**: a `CCall` to a `CFn` (symbol, C signature, library); lowering takes addresses of
  borrowed arrays and strings, makes `Out`/struct temporaries, takes handle references, records
  the task, captures `errno`, and emits trampolines; `declare c async` lowers to a pool call or a
  completion.
- **codegen**: a direct call with the target's C convention in both backends, aggregates by the
  shared classifier.
- **link**: the libraries' archives or shared libraries and the system libraries.
- **WebAssembly**: no libraries but `c` under WASI in the first steps.

## Cost

A `declare c` call costs a C-to-C call plus two thread-local stores (the task record), and one
that asks for `errno` a call and a store more. `StrBytes`, borrowed arrays and synchronous leases cost nothing at run time; a
handle argument one atomic increment and decrement; `Out<T>` a stack slot. A queued callback a
few atomics and a copy into its ring.

Gates: `packages/sqlite` on `declare c` reaches rusqlite on every `bench/db` SQLite workload and
beats better-sqlite3 (per workload, instructions and wall clock, arm64 and x86-64); Postgres and
Redis within ±3% of today or faster after their move (and the Velt drivers of the follow-up
track matching or beating them); std's file I/O unchanged after the libc
fold; every other benchmark within 3%.

## Diagnostics

- `calling \`sqlite3_step\` is unsafe: wrap it in \`unsafe { … }\` with a \`// SAFETY:\` comment`.
- `this unsafe block has no \`// SAFETY:\` comment saying why it is sound`.
- `unsafe code needs a native package: \`unsafe\` is allowed only in packages with \`native.c\``.
- `package \`llama\` (through \`chat-kit\` → \`llama\`) has native code; add it to \`allowNative\`
  in package.vlt: allowNative: ["llama"]`.
- `\`Ptr<sqlite3>\` can't be imported here: raw pointers stay among native packages`.
- `\`sqlite3_bind_text\` parameter 4 is \`int\` in sqlite3.h:4581; \`n: i64\` declares
  \`int64_t\`: use \`i32\``.
- `\`sqlite3_bindtext\` is not declared in sqlite3.h; did you mean \`sqlite3_bind_text\`?`
- `\`string\` cannot be passed to C here: declare the parameter \`StrBytes\` or pass a
  \`CString\``; `\`StrBytes\` is borrowed for a synchronous call; an async call takes a
  \`CString\``.
- `this native callback captures \`rows\`, which its creator still uses; move it in or make it
  \`shared\``.
- `\`logits\` is lent to native code until \`lease\` ends (line 12); it can't be read here`.
- `the statement handle was closed` (`HandleClosed`, at run time).
- `\`sqlite3_open_v2\` has no library on \`wasm32-wasip1\``, naming the caller.

## Steps

1. **The language**: `unsafe` blocks and functions, the `SAFETY` lint, the trust boundary and
   `allowNative` (templates, examples and playground updated); `declare c`, `declare c type`,
   `declare c const`; scalars, `Ptr`, `null`, arrays and views, `StrBytes`, `CString`, `Out`, raw
   reads; `errno` capture; system libraries; the header check with the digest and `unchecked`;
   effect summaries; `velt check`'s listing; LLVM, Cranelift and the dev JIT; the reference entry
   and its docs-sync issue.
2. **libc**: std's `declare system` uses become `declare c("c")` with the guards in Velt, checked
   against each CI target's headers; the compiler's table goes. Gate: file I/O unchanged. Phase
   3 of #2196 (the OS boundary and the allocator through `declare c`) builds on steps 1 and 2.
3. **Bundled sources and SQLite**: `sources` built and cross-built reproducibly with `zig cc`,
   checked against their headers; `NativeHandle`; `packages/sqlite` moves onto `declare c` and is
   gated against rusqlite and better-sqlite3; its Rust crate is deleted.
4. **C structs**, entered by the differential ABI test passing in both backends on every target.
5. **`NativeCallback`**: task-keyed delivery, bounded rings, transfer-checked captures (the
   `Channel` idiom documented), `CFunction`.
6. **Async**: library pools, `PinnedBuffer` and leases, `Completion<R>` and `velt.h`; a llama.cpp
   package as the worked example (tokens/s against `llama-cli`, thread counts reported).
7. **Rust through `declare c`**: `crate` libraries with generated headers and the slimmed
   `velt_native`; Velt's reactor in `velt.h` with the SDK's `AsyncRead`/`AsyncWrite` and spawner;
   `packages/postgres` moves to `declare c` and the reactor API (gate: ±3% of today or faster on
   its `bench/db` rows; `packages/redis` needs neither, it is plain Velt now), with one worked example of an off-the-shelf
   trait-based crate; then the Velt-specific native ABI, native_abi.md and their tests are
   removed.
8. **Per-platform backends**: a BLAS package on Accelerate, OpenBLAS and MKL, benchmarked on
   `sgemm` against each library called from C.
9. **The main thread**: a task pinned to the process's main thread, driven by a platform loop in
   turns as #1737 drives the WebAssembly executor; for GUI bindings. Designed with the GUI work.
10. **Bindings from headers**: a source generator (`declare generated`) in a package that reads
    clang's JSON AST and writes declarations that pass the check by construction.

## Rust crates on Velt's scheduler (step 7)

Today's Rust driver (`packages/postgres`, as `packages/redis` was) runs on Velt's scheduler through the
native ABI's async I/O part (`velt_native::net::TcpStream`, `velt_native::rt::spawn`: the
program's own sockets, timers and tasks, polled inline by the awaiting task). With that ABI
retired, `velt.h` keeps them there (owner, 2026-11-03: option (a); no option may regress
performance):

- **Velt's reactor as public C.** `velt.h` exposes registering a socket with the current
  worker's I/O driver (readiness, read and write), wakers, spawning onto the program's scheduler,
  and timers: the async I/O part of today's Rust-only ABI made public C, the same move as
  `Completion`.
- **The Rust SDK** (`velt_native`) wraps it and implements the standard async traits on top:
  `tokio::io::AsyncRead`/`AsyncWrite` and `futures::io` for Velt sockets, plus a spawner. Crates
  generic over those traits rather than tied to tokio's runtime then run on Velt's scheduler with
  no thread hops: `tokio-postgres` through `connect_raw(stream)`, hyper on any `AsyncRead +
  AsyncWrite`, most protocol crates that take a stream. Wrapping a Rust crate stays the quick way,
  and stays fast.
- **Crates hard-wired to `tokio::spawn`/`tokio::net`** fall back to their own runtime and
  `Completion` (option (b), a fallback only). The guide documents the cost (two thread hops per
  call) and how to avoid it: the crate's trait-based or `connect_raw`-style API, or a small
  adapter.
- **Cancellation**: a socket or task registered through the API belongs to the awaiting task's
  scope and is closed or cancelled with it, by the rule of `Completion` and leases; late events
  after cancellation are dropped, never delivered to freed state.
- **Gate before the old ABI is removed**: the Rust `packages/postgres`, ported to `declare c` and
  the public reactor API, runs within ±3% of today on the `bench/db` Postgres rows; plus one worked example of an off-the-shelf trait-based crate wrapped
  this way.

## Follow-up track: first-party drivers in Velt

Separate from this design's steps and not a step-7 blocker (owner, 2026-11-03: option (c)):
Postgres and Redis clients written in plain Velt on `velt/net`, as Node's `pg` and `ioredis` are
plain JavaScript: protocol clients, not database rewrites. They run in the task with no FFI and
no thread hops, and prove that `velt/net` and Velt's async carry real database protocols.

- Redis's RESP first (small), then Postgres: startup, SCRAM, the extended query protocol, type
  codecs, TLS, COPY.
- **Gate**: match or beat the Rust drivers on the same `bench/db` rows. When a Velt driver
  passes, the Rust one is retired.
- **As built, Redis:** `packages/redis` passed and its crate is gone. A command queues its bytes and
  its place in the line for replies under a short lock, and its own task sends them, so it takes no
  hop through another task; one reader task per connection completes the line. Best of 13 interleaved
  rounds, Linux, one compiler, Velt over Rust: `set_seq` 1.006, `get_seq` 1.003, `pipeline_set`
  1.027, `pipeline_get` 1.034, `concurrent_get` 1.069. What the driver needed to get there: wait
  before reading (a read per reply found nothing), no driver task, one-shot channels reused rather
  than made per command. Channel operations are what cost most in `velt_rt`; Postgres will meet the
  same.

## As built

Step 1 lands in layers, each its own pull request:

1. `unsafe` blocks, expressions and functions (#2201).
2. `declare c("lib") function` with scalars (integers, `f32`, `f64`, `bool`, `void`): a direct
   call in both backends and the dev JIT (which finds the C library's functions with `dlsym`),
   `errno` captured after it on Unix for a declaration that asks (on Windows, `lastOsError()` after a C call reads
   `GetLastError()`, which the C library doesn't set, so it does not report the call's error
   until step 2 captures the CRT's `errno`), calls
   only in unsafe code. Until the trust boundary layer, a C declaration is allowed in std and in
   any file with a package (not a lone file), the only library is `"c"`, the platform's C
   library, and a call on WebAssembly is an error. One symbol has one signature: a second one
   (another `$suffix`, another package) is an error until calls through C function pointers.
   Under `velt dev` a C function no loaded library has is an error naming its declaration.
   Step 1 is complete only with layers 5 (the trust boundary) and 6 (the header check): until layer
   6, declarations are unchecked.
3. `Ptr<T>` (a std struct over the address; `Ptr.at` is unsafe), `Ptr<T> | null`, `null`,
   `declare c type`, arrays of numbers and views of bytes as buffers (read at the call, after
   every argument), `StrBytes` (a well-formed string's own bytes; a std hook makes the U+FFFD
   copy otherwise) and `byteLength`. Lowering converts pointers through std hooks
   (std/ffi/ptr.vlt), which LLVM inlines. A `declare c type` can't be made with `new`; an empty
   array reaches C as a dangling non-null address; a `Ptr<T>` result (not `| null`) that is NULL
   stops the program, naming the C function, since C has no non-null pointer type a header
   could state.
4. `CString` (a class over its NUL-terminated bytes, refusing a NUL inside), `Out<T>` for
   numbers and `OutPtr<T>` for pointers (C writes the field whose address it gets: a
   `Ptr<T> | null` field isn't a C pointer, so pointers have their own class), and the raw reads
   `readBytes`, `readUtf8` and `readCString` (unsafe functions in Velt, over the std-only
   intrinsic `addressLoad`; `readCString` calls `strlen` through `declare c("c")`). `Ptr<T>` and
   `OutPtr<T>` point at a C type, a number, `bool`, `void` or another pointer. `CString.bytesOf`
   lends its bytes read-only, zero-copy (Rust's `as_bytes`); #2228 closes the `readonly` gaps
   that let a write through them.
5. The trust boundary, in two parts.
   - 5a: `allowNative` and `native.c`'s `system` libraries.
     - A package declaring C functions has `native.c`, `c: {}` when it calls only the C library;
       without it, its first declaration is an error. So its manifest says it has native code.
     - Installing a package with native code (a Rust crate or `native.c`) that the root doesn't
       list is an error naming the dependency chain and the line to add. The compiler checks
       the same at each package's first C declaration.
     - A library other than `c` is one of the package's `native.c` entries. Its `system` object
       maps build conditions (`native`, `darwin`, `linux`, `win32`, `default`) to a library name
       or, under `darwin` only, `framework:Name`, checked to be a name of at most 64 bytes and
       never a linker flag or a library's file. The first condition the build has applies. A
       target with none is an error at the declaration.
     - Every build links the libraries the program's declarations name (`-lname`, `-framework
       Name`, `name.lib`), and `velt dev` opens them (`dlopen` with `RTLD_GLOBAL`,
       `LoadLibraryA`).
     - The bundled linker's kits have only the platform's own libraries, so another system
       library links with the system linker (`VELT_LINKER=system`).
     - `sources` entries came with step 3 (layer 7); `crate` entries come with step 7.
   - 5b: the code that needs trust stays in allowed packages and std.
     - `unsafe` blocks, expressions, functions and methods are errors outside std and the
       packages with native code that the program allows (a program's own files included, unless
       the program's own package.vlt declares native code; and code hidden in a default value).
       veltc checks the loaded modules' ASTs (`native/trust.rs`); sema already requires an unsafe
       context for each unsafe call.
     - An npm package isn't part of the package around it: a file under a `node_modules`, or
       under a directory below the package's root with its own `package.json` or package.vlt,
       has no package's trust (`PackageGraph::package_of`).
     - What the AST check can't see stays out of reach another way. Dynamic dispatch (`any`,
       `Proxy`, `Reflect`) doesn't reach unsafe methods and accessors: their names give a
       `TypeError`. The methods the language calls implicitly (symbol-keyed ones, `toString`,
       `toLocaleString`, `valueOf`, `toJSON`, `then`) can't be unsafe. A generator's output
       can't contain `unsafe` code. `Ptr`'s address is a `readonly` field, which a write through `any`
       refuses; `OutPtr`'s and `CString`'s are private fields of std classes, which are no members
       through `any`.
     - The raw API, `Ptr`, `OutPtr`, `declare c type`s and unsafe functions, can be imported
       there only: by name, re-exported (through any number of modules), or as a namespace
       import's member (`ffi.Ptr`). A safe function of an allowed package can still return a
       `Ptr`; the code receiving it can't read or write through it without `unsafe`.
     - The `SAFETY` lint: in those packages each unsafe block and expression has a comment with
       `SAFETY:` in the comment lines directly above its line, and each unsafe function such a
       comment or an `Unsafe:` paragraph in its doc comment (what callers must guarantee). It is
       an error. std follows it too, checked by a test rather than at each build.
     - `addressLoad` is `declare unsafe intrinsic function`: its callers are unsafe code.
     - The language server doesn't run these checks yet (builds and `velt check` do).
   - 5c: what the program trusts, in view.
     - `velt check` ends, when it passes, with each package with native code that the program
       allows: its C libraries (with the system library each links on the target), whether it
       has a Rust crate, and each piece of its unsafe code by file and line. `--json` has the
       same under `native`. Nothing is listed for a program without such packages.
     - `velt.lock.json` records the root's `allowNative` (sorted), so a change of trust shows in
       the lockfile's diff and `--locked` refuses a lock that doesn't have it. A workspace's
       lock has none, as its install has no root.
     - Not yet: header-checked or `unchecked` libraries (layer 6) in the listing.
6. The header check and `declare c const`, in parts.
   - 6a-1: what the check needs.
     - `native.c.<library>.headers`: the package's own headers (`native/sqlite3.h`) or C
       library headers (`<string.h>`), one list for every target. `native.c.c` lists the
       platform's C library's. `unchecked: true` says a library has none; `allowNative`'s
       error and `velt check` show it, and `velt check` shows each library as checked against
       headers, unchecked, or without headers.
     - zig 0.17.1, pinned with each host's SHA-256 (`velt_toolchain::zig`), downloads into
       `$VELT_HOME/cache/zig/` on first use (about 55 MB, 100 MB on Windows, 10 s), or `$VELT_ZIG` names one (CI,
       offline machines).
     - CI: the `c-headers` job checks every target from one Linux machine, only when a change
       touches what it checks (`crates/xtask/src/c_headers.rs`), so nothing else
       downloads zig; the nightly runs it whatever changed. Without `VELT_ZIG` the check
       fails in CI instead of skipping, and the other test runs leave it out.
   - 6a-2: declarations checked against headers in the packages a build has locally (the
     root, path dependencies, workspace members); registry and git packages come with 6b's
     digest.
     - zig's `zig cc -###` gives the clang command for the target; that command with
       `-ast-dump=json` in place of the object parses the headers (`zig cc -fsyntax-only` still
       renames an object it never made).
     - Each declaration must name a function the headers declare, with as many parameters, not
       variadic, and each parameter and the result must match per the table above (typedefs
       resolved, `long` per the target's data model, clang's predefined `__size_t`). Windows
       targets read zig's MinGW headers, which have MSVC's type sizes. An enum is as wide as
       its fixed type or its widest constant; a packed enum, one with `mode(...)`, and one whose
       definition clang's dump doesn't show (only declared, or defined in a prototype) can't
       cross. A function whose type has a calling convention other than the target's C one
       (`vectorcall`, `pcs(...)`, `preserve_most`, `ms_abi` off Windows, `sysv_abi` on it) is
       refused; `cdecl`, 32-bit x86's conventions (which 64-bit targets ignore) and `noreturn`
       aren't conventions here.
     - `<name.h>` means a header of zig's set: the C library's for each target. Other libraries'
       headers (`<sqlite3.h>`) aren't in it, and nothing reads the machine's own: a package ships
       its copy (`native/sqlite3.h`).
     - A check that passed is remembered in `$VELT_HOME/cache/headers/`: keyed by zig's version,
       the target, the package's root, the headers' names and the declarations, it holds a
       digest of every file the parse read (clang's dependency list), so what a listed header
       includes counts. clang runs again when one changes (about 0.2 s per library). The
       package's files among them are build-stamp inputs, so a changed header rebuilds (and so
       checks) a program whose sources didn't change.
     - Not yet: headers per build condition (frameworks: zig has no macOS SDK), structs,
       function pointers (other than as `Ptr<void>`, 6c-1), variadics.
   - 6b: what installs check.
     - A C library a package declares functions of lists its `headers` or says
       `unchecked: true`; `c`, the platform's C library, too (`c: { c: { headers:
       ["<string.h>"] } }`). One doing neither is an error where it is declared.
     - `velt native headers` checks the package on every target with C headers (macOS, Linux
       gnu and musl, Windows; x86_64 and aarch64) and writes `c-headers.json`: per zig target
       and library, a SHA-256 of each declaration (its name and the Velt types its parameters
       and result pass) and of each of the package's own headers the parse read, with what they
       include. Nothing is written when a check fails, or when there is nothing to record.
       `velt publish` runs it for a package with `native.c`; git packages commit the file. It is
       part of the package's contents.
     - A fetched package (registry, git) is compared with its record for the build's target. A
       missing record or target is an error. Each declaration the program compiles must be in
       the set: a program loads some of a package's modules, so the set can hold more. Its
       `unchecked` libraries aren't compared.
     - Each recorded header the package has must have its digest (line endings aside, which a
       checkout may change). A git dependency has the commit's whole tree, so a recorded or
       listed header it lacks is an error too.
     - A registry archive ships `native/` only for a package that builds C `sources`; there a
       recorded or listed header under `native/` it lacks is an error too.
     - **Limits.** Other registry archives don't ship `native/`, so there the headers can't be
       compared: a registry package is trusted to have run the check on the headers it
       published. And the record isn't a signature: an author can write any digest, so it catches
       mistakes, not a hostile package, which is what `allowNative` is for.
     - `velt check` says "headers checked" only for a library that passed here on this target,
       by the check or by its record; on a target without C headers (WebAssembly), or with
       nothing of the library declared, it says "headers listed".
   - 6c-1: `declare c const NAME: T;`, a constant whose value comes from the headers per target.
     - `T` is an integer, `f32`, `f64`, `bool`, `Ptr<U>` or `Ptr<U> | null`. It has no library of
       its own in the syntax: its package's libraries that list `headers` are read in turn, and
       one that has other value than another is an error. A package has to be allowed to declare
       one as for a C function. A library that says `unchecked: true` has no headers to read, so
       no constants; nor has std, without a manifest (step 2).
     - clang evaluates what C can: an object-like macro, an enumerator, a `static const`.
       `__typeof__(NAME)` in a generated file says it exists and its C type, an enumerator
       initialized from `(intmax_t)(NAME)` holds its value in the AST dump, and a float comes
       from the LLVM IR of a constant initialized from it. A function-like macro or a name
       headers don't have is "not defined", a variable or a string "has no value a program can
       use".
     - The value decides for integers (a flag `#define`d as `int` can be a `u32`), any that fits;
       a float type takes its C type's (`f32` a `float`, `f64` a `float` or `double`), `bool`
       `_Bool`, a `Ptr` a pointer of its C type (NULL only if `| null`). Reading one needs no
       `unsafe`: the value is what the headers say, and a `Ptr` can't be read through without it.
     - **Until `CFunction<F>` (step 5), `Ptr<void>` matches a function pointer**, in `declare c`
       function parameters too, or `sqlite3_bind_text` and `SQLITE_TRANSIENT` couldn't be
       declared against `sqlite3.h` (`declare c const SQLITE_TRANSIENT: Ptr<void>;`).
     - Values go by the target: `velt native headers` records each in `c-headers.json`, per
       target in `constants` beside `checked`'s digests: the type declared, the C type and the
       value, which a fetched build compiles in, and a digest of each of the package's own
       headers clang read. A fetched package's come from there: a declaration that differs, a
       value its type can't hold (checked as the author's was, with the recorded C type), or a
       missing target is an error, and its headers are compared as a library's are. What clang
       said is remembered like a check, with the files it read, and holds while they do; the
       package's own files among them are build-stamp inputs, so a changed header changes the
       program with no source changed.
     - HIR: `GlobalDef::c_const` holds the value, set by the driver after sema; lowering reads it
       instead of the initializer, in tables too. On a target without C headers (WebAssembly) a
       reachable read is an error, as a C function's call is.
     - Not yet: strings. A macro of a string literal is an error.

7. Step 3's first part, C `sources`.
   - `native.c.<library>.sources` (`.c` files under `native/`) with optional `defines`: one source
     per library, so not with `system`; C++ and Zig sources, a system library a source needs beyond
     what every program links, and a crate beside `sources` come later. A package with `sources`
     ships all of `native/`, says `targets` beside `c`, and has a scoped name no more than a crate
     does.
   - `velt native build [--target]` compiles with the pinned `zig cc`, for any target from any
     machine (macOS 11 on arm64 and 10.12 on x86_64, glibc 2.17, so a library neither raises what a
     program needs nor depends on the builder) into a bundle: `-O2 -g0 -fPIC`, `-ffunction-sections`,
     no sanitizer, run in the package's root with relative paths and `SOURCE_DATE_EPOCH=0`. Two
     builds, in other directories, give the same bytes; a test compares them per target. On
     Windows clang records zig's random temporary path in each object whatever `-g0` says, so
     what is reproducible there is what ships: the DLL, linked with `-s`, and its import library.
   - The bundle is reused while a hash of what it's built from is the same (`<out>.key` beside
     it): zig's version, the flags, target, package name and version, each library's sources and
     definitions, and every file under `native/` (a change to an included file rebuilds), never
     their times. An unchanged build runs no zig. A library is built when a program links or
     loads it (`native::c_sources`), not at install: `velt check`, `velt native headers` and the
     editor never compile C, and a package's libraries its program doesn't call aren't built or
     linked.
   - Linked as the design says: debug the shared library (by `-l`, with an rpath), release the
     static one (`static/lib….a`, as an archive so unreferenced objects stay out), and a musl
     program always the static one. **Windows links the DLL in both modes**, as a Rust crate does:
     zig's objects for Windows are MinGW's (the headers `headers` reads too), and a DLL has its own
     C runtime behind its import library; a static library for MSVC's linker needs more. `velt dev`
     loads the shared library with `RTLD_GLOBAL`, so `dlsym` finds its functions; a change under
     `native/` restarts the host, as for a crate.
   - `velt publish` builds every `targets` entry (`--native-artifacts` still wins) and refuses a
     missing one or none; the bundle goes through the registry's paths, checksums and lock
     entries like a crate's. Those who install it fetch and verify it and never run zig. A
     registry package with no bundle for the target, and a git package (which has none), build
     here with the pinned zig only with `VELT_NATIVE_FROM_SOURCE=1`: compiling C runs no code of
     the package's, but it downloads zig and trusts a build no checksum pins, so it is asked for
     as for a crate. `--macos-sdk` waits for sources that link frameworks.
   - `velt check` lists the sources (`tiny (built from native/tiny.c; headers checked)`, `sources`
     in `--json`); `allowNative` asks for such a package as for any with native code; the header
     check reads a library's own `headers` with its `defines`.
   - Not yet: the registry or CI rebuilding a bundle and comparing checksums (what reproducibility
     is for); `packages/sqlite` on `sources`. Sources fetched from `git` or a `url` with a pinned
     hash are #2367's: its `fetch` is a key beside these fields, and only changes where `sources`
     are read from.

## Decisions

- `declare c("lib")` (it names the ABI); one way to call native code, Rust included; `declare
  system` folds into `declare c("c")` (owner).
- `unsafe` blocks and functions with `SAFETY` comments, only in `allowNative` packages and std
  (owner); `unsafe` functions and raw types importable only by `allowNative` packages.
- Declarations checked against headers from step 1; `unchecked: true` without headers (owner).
- `NativeHandle` after .NET's `SafeHandle` (owner).
- `Completion<R>` and a public `velt.h` for non-blocking async (owner).
- Rust drivers' I/O: Velt's reactor in `velt.h` with the SDK's async traits (step 7), own runtime
  only as a documented fallback; first-party drivers in Velt as a separate track (owner).
- `allowNative` is an install error from the start, no warning release (owner).
- Re-entrancy: callback captures follow the transfer rules; async leases, handle references and
  completions move into the call (review).
- User-set build conditions: a separate change. A result-returning callback with no active C
  call stops the program until step 9.
