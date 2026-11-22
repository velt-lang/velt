# Native code through `declare c` and `unsafe`

Status: partly implemented. Issue: [#2187](https://github.com/velt-lang/velt/issues/2187).

## Summary

`declare c` is the one way Velt calls native code, whatever language the library is written in:
the platform's C library, C sources bundled with a package, or a system library. The parts:

1. **`declare c("lib")` declarations**, checked against the library's C headers per target. The
   manifest maps each logical library name to a library per target; `"c"` is the platform's C
   library.
2. **`unsafe { … }` blocks and `unsafe function`s** mark every call into native code and every
   raw operation, each with a `// SAFETY:` comment saying why it is sound.
3. **A trust boundary**: only std and the packages with native code that the program lists in
   `allowNative` may contain unsafe code or import the raw types, so user code works with a
   binding's safe API and an audit reads exactly the marked places.
4. **`@export("c_name")`**: the other direction, a Velt function C calls by name or address.

The shape follows C#'s: `unsafe` blocks allowed per project, `[LibraryImport]` declarations,
per-platform library resolution and `SetLastError`. Velt is stricter where it can be: there is
no run-time marshalling layer, declarations are checked against headers, and the consuming
program opts in to each package's native code explicitly.

How a package ships a Rust crate with a Velt-specific ABI is a separate mechanism, described in
[native-packages.md](native-packages.md).

## Motivation

A C library has no table of signatures in the compiler: the declaration itself is the statement
of the C signature, and a wrong one corrupts memory. So the design has three parts: **what
crosses** (types and their lowering), **checking the declaration against the library's
headers**, and **marking and confining the code that relies on it** (`unsafe`, `allowNative`).

`packages/sqlite` is the worked example: it calls SQLite's C API directly, each call one direct C
call with no glue layer, no handle table and no lock in between, and passes strings without
copying them:

```ts
// packages/sqlite/src/sqlite3.velt (excerpt)
import { CString, Out, Ptr, StrBytes } from "velt/ffi";

export declare c type sqlite3;
export declare c type sqlite3_stmt;

export declare c("sqlite3") function sqlite3_open_v2(
  name: CString,
  db: Out<Ptr<sqlite3> | null>,
  flags: i32,
  vfs: null,
): i32;
export declare c("sqlite3") function sqlite3_bind_text(
  s: Ptr<sqlite3_stmt>,
  i: i32,
  v: StrBytes,
  n: i32,
  destructor: Ptr<void>,
): i32;
export declare c const SQLITE_OK: i32;
export declare c const SQLITE_TRANSIENT: Ptr<void>;   // the value comes from sqlite3.h
```

```ts
// packages/sqlite/src/connection.velt (excerpt)
const out = new Out<Ptr<sqlite3> | null>(null);
// SAFETY: the name is NUL-terminated, and `out` is written before the call returns.
const rc = unsafe { sqlite3_open_v2(name, out, flags, null) };
const db = out.value;
```

The `unsafe` blocks are the package's whole trusted base, each with its reason; the exported
classes are the safe API.

## Design

### Declarations

`declare c("<library>") function name(params): R;` is a declaration kind of its own in the AST,
HIR and VIR. The keyword is `c` because it names the ABI, which every language here speaks.

- `<library>` is a **logical name** that the package's manifest maps to a library per target
  ("Where libraries come from"); `"c"` is the platform's C library. The compiler never names a
  platform or a library.
- The link symbol is `name`; a `$suffix` is ignored, so one module can declare one symbol with
  several signatures (`memcpy$sched`).
- **`declare c type sqlite3;`** declares a type that exists only behind pointers: `Ptr<sqlite3>`
  and `Ptr<sqlite3_stmt>` are different types. A `declare c type` cannot be made with `new`.
- **`declare c struct S { … }`**: a C struct ("C structs").
- **`declare c const NAME: T;`**: a constant whose value comes from the headers per target
  ("Constants from headers"), never written by the binding: a wrong flag is as unsafe as a
  wrong signature (`SQLITE_STATIC` for `SQLITE_TRANSIENT` is a use-after-free).
- **`declare c("<library>") let name: T;`** (or `const`, read-only): a variable the C library
  defines ("C variables").
- Variadic functions end with `...args: CVarArgs` ("Variadic functions").
- A declaration is allowed on every target; a reachable call where the manifest maps no library
  for the target is an error naming the target and the caller, so portable code chooses per
  platform with conditional imports.

Declaring is safe; calling needs `unsafe`.

### `unsafe` blocks and functions

```ts
// SAFETY: `p` comes from sqlite3_prepare_v3 and is finalized once.
const release = (p: Ptr<sqlite3_stmt>) => unsafe { sqlite3_finalize(p) };

/**
 * Unsafe: `p` points at `n` readable floats.
 */
unsafe function sum(p: Ptr<f32>, n: usize): f32 { … }
```

- **Syntax.** `unsafe { … }` is a block statement, read like `try { … }`. In expression position
  `unsafe { expr }` holds one expression and evaluates to it. `unsafe` is a modifier on
  functions and methods, read like `async` (`unsafe function`, `unsafe async function`,
  `static unsafe m()`), and an unsafe function's body is unsafe code. `unsafe` is a contextual
  keyword: it stays usable as an identifier where neither `{` nor a function follows it on the
  same line. Interface members cannot be `unsafe`.
- **Unsafe contexts are lexical.** Functions written as expressions inside an unsafe block
  (arrows, function expressions, object-literal methods) are unsafe code; nested function
  declarations and class members are not, nor are an unsafe function's default parameters.
- **Unsafe functions are not values.** A function type cannot say it is unsafe, so an unsafe
  function used as a value (not called) is an error; a safe arrow that calls it in an `unsafe`
  expression is the value.
- **What requires `unsafe`**: calling a `declare c` function, reading or writing a C variable,
  and calling an unsafe function, which includes velt/ffi's raw operations (`Ptr.at`,
  `readBytes`, `readUtf8`, `readCString`, `errno`, `setErrno`) and std's `addressLoad`
  intrinsic.
- **What stays safe**: making a `CString` or an `Out`, passing a `string` as `StrBytes`,
  reading a `declare c const`, and everything type-checked.
- **`// SAFETY:` comments.** In packages, each unsafe block and expression needs a comment
  containing `SAFETY:` in the comment lines directly above it, and each unsafe function such a
  comment or an `Unsafe:` paragraph in its doc comment saying what callers must guarantee. A
  missing one is an error (`crates/veltc/src/native/trust.rs`). std follows the same rule,
  enforced by a test rather than at every build.

### What crosses

| Velt | C | Notes |
|---|---|---|
| `bool`, `i8`…`i64`, `u8`…`u64`, `isize`, `usize`, `f32`, `f64` | `_Bool`, `int8_t`…, `ssize_t`, `size_t`, `float`, `double` | by value; `bool` is C's `_Bool` only, never an `int` flag |
| `Ptr<T>`, `Ptr<T> \| null` | `T *` | an address; `null` is `NULL` |
| a `@repr("transparent")` struct (velt/compiler) | its one field's C type | exactly as the field, on every target: `struct Meters { readonly m: f64; }` is a `double`, not a C struct of one (which Windows x64 passes in an integer register) |
| `null` (as a parameter type) | any pointer | always `NULL` |
| `readonly T[]`, `T[]` (`T` a number type), views of bytes | `const T *` / `T *` | the first element's address, borrowed for the call; the binding passes the length |
| `StrBytes` | `const char *` | a `string`'s UTF-8 bytes, borrowed, no NUL; the binding passes `byteLength(s)` |
| `CString` | `const char *` | an owned NUL-terminated copy (`new CString(s)`, refusing a NUL inside) |
| `Out<T>` | `T *` | the `value` C writes through the pointer: a number, `bool`, or `Ptr<U> \| null` for a `U **` |
| `void` | `void` | as a result |

- **No hidden conversions.** A `string` crosses only as a `StrBytes` parameter (the declaration
  says so) or through a `CString` the binding made. A `string` passed to any other parameter is
  an error that names `StrBytes`.
- **Strings to C without a copy.** Velt strings are stored as UTF-8 (canonical WTF-8,
  [rt_abi.md](../contracts/rt_abi.md) "Strings"), so a `StrBytes` argument is the string's own
  bytes. A string whose header says it may hold a lone surrogate is copied first with U+FFFD for
  each; U+FFFD is 3 bytes like the surrogate, so `byteLength(s)` stays exact. Well-formed strings
  never copy.
- **Arrays** are read at the call, after every argument is evaluated, and borrowed for its
  duration: the array cannot grow, shrink or move while C holds the address, and C may not keep
  it. A `T[]` parameter not written `readonly` is a mutable borrow, refused for a read-only
  array. An empty array crosses as a non-null address aligned for the element type; only `null`
  is `NULL`.
- **Pointer results.** A `Ptr<T>` result (not `| null`) that is NULL stops the program, naming
  the C function, since C has no non-null pointer type a header could state.
- **Strings from C** are read explicitly, in unsafe code: `readUtf8(p, len)`, `readCString(p)`
  and `readBytes(p, len)` copy; invalid UTF-8 becomes U+FFFD. C keeps ownership of `p`.
- `Ptr<T>` points at a `declare c type`, a number, `bool`, `void`, a C struct or another
  pointer; a pointee that is a type parameter is checked where it is instantiated. It is a type
  of its own (`hir::TyKind::Ptr`, declared in `std/ffi/ptr.velt`), one machine word, and
  `Ptr<T> | null` is the same word with 0 as `null`, so nothing converts at a C call. Nothing
  puts 0 in a `Ptr<T>`: `Ptr.at(0)` stops the program (`Ptr.orNull` gives `null` for 0), and on
  wasm32 so does an address past 32 bits.
- **`Out<T>`** takes a number, `bool` or `Ptr<U> | null` only: C may write NULL into a non-null
  pointer, and writing a class reference, a `symbol` or a `shared<T>` would forge one.
- **A pointer in the language** is Copy, prints as `Ptr(0x7ffd5e8c1a30)`, compares and hashes by
  address, narrows and is truthy, and crosses workers as it is. Text of it (`${p}`, also as an
  element, `${[p]}`), `typeof`, ordering, arithmetic, JSON, a pointer to another C type, and
  conversion to or from `any`, `unknown`, `object` or an interface are compile errors, through
  a type parameter too; `Ptr.addressOf(p)` gives the address. A class or array holding one
  still converts to `any`, as one holding a SIMD vector does.

### Errors: `errno` and `GetLastError`

A declaration asks for the error, as .NET's `SetLastError` does per P/Invoke:

```ts
declare c("c", { errno: true }) function open(path: CString, flags: i32): i32;
```

Right after a call of such a function, lowering stores the C library's `errno` into the thread's
error slot, std's `ThreadLocal<i32>` `OS_ERROR` (`std/os/rt.velt`): `*__error()` on Apple
platforms, `*__errno_location()` with glibc and musl, `*_errno()` from the Universal CRT for the
library `c` on Windows, and `GetLastError()` for any other library on Windows. No runtime code
runs between the call and the store. `lastOsError()` reads the slot; it is meaningful only after
a call that asked, since any other call leaves the slot alone. The accessor per target is
`crates/velt_vir/src/c_abi.rs`, the one fact about the C library that lowering keeps.

Capture is off by default because its cost is a call and a store per C call, and many libraries
report failures through results and never need it (SQLite's range select of a hundred rows costs
9% more with it). Whether a call captures depends only on its declaration, never on whether
the program reads `lastOsError()` elsewhere.

Wrappers that need `errno` only on failure declare the function without `errno: true` and read
it themselves when the call returned -1 (std's `keepOsError`, `std/os/rt.velt`), so successful
calls pay nothing.

### Setting `errno`

Code that must restore the live `errno` (a signal handler returns with the `errno` it found)
reads and writes the C library's own through velt/ffi's unsafe `errno()` and `setErrno(value)`.
They are std code over `declare c`: each C library gives the address of the calling thread's
`errno` through a function of its own (`__error()`, `__errno_location()`, `_errno()`), chosen by
the conditional import `#errno-location` (`std/imports.velt`). Where there is none
(WebAssembly, JS hosts) `errno()` is 0 and `setErrno` does nothing. `errno` itself is a
thread-local variable, which a C variable declaration cannot name.

### C variables

A C library's variable is declared like a function of the library, with `let` (the program may
assign it) or `const` (only read):

```ts
declare c("c") let environ: Ptr<Ptr<u8> | null>;
declare c("tiny") let verbosity: i32;
declare c("tiny") const version_number: u32;
```

- **Syntax**: a module-level declaration with no initializer (C defines it) and no options. The
  parenthesized library is the difference between `declare c const NAME` (the header's
  compile-time value) and `declare c("lib") const name` (a variable in the library's memory,
  read at run time).
- **Types**: an integer, `f32`, `f64`, `bool`, `Ptr<T>` or `Ptr<T> | null`.
- **Use**: a read is a load from C memory at each use (never narrowed, never folded), and
  `x = v` or `x op= v` stores there; both need unsafe code, since C or another thread may change
  it at any time. A `const` one cannot be assigned. A `Ptr<T>` one that holds NULL stops the
  program at the read.
- **Lowering**: one `Import` static per symbol, with no contents (`vir::StaticData::c_global`).
  LLVM declares an `external global`; Cranelift a writable data import. On Windows every C
  library is a DLL, and a DLL's variable is reached through the import library's
  `__imp_<name>` pointer, so lowering takes that symbol and loads the address from it; no backend
  needs `dllimport`. For `velt dev` the driver lowers with `LowerOptions::jit`, which names the
  variable itself, since only a linker makes `__imp_<name>`.
- **Header check**: the headers must declare a variable of that name with external linkage and
  the matching C type. A `let` of a `const` C variable is an error, and so is a `static` one (no
  link symbol) or a thread-local one (each thread has its own, which a symbol does not name).

### Variadic functions

C's `...` (`fcntl`, `open`, `ioctl`, `printf`) is the declaration's last parameter,
`...args: CVarArgs`:

```ts
import { CVarArgs } from "velt/ffi";

declare c("c") function fcntl(fd: i32, cmd: i32, ...args: CVarArgs): i32;

// SAFETY: `fd` is open; F_SETFL takes an `int`.
unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) };
```

- **What a call passes.** The fixed parameters are checked as any C call's. Each further argument
  is an integer, `f32`, `f64`, `bool`, `Ptr<T>`, `Ptr<T> | null` or `null`. Anything else is an
  error naming its type (a `string`, an array, a struct, a spread). A number literal has no type
  of its own there, so it is an error with the fix (`0 as i32`, `1.5 as f64`): C reads the bits
  as the format or the command says, and a guessed `f64` where C reads an `int` is a wrong value.
- **C's default argument promotions** happen in lowering (`crates/velt_vir/src/lower/c_call.rs`):
  an integer narrower than `int` and `bool` pass as an `int`, extended by their own signedness,
  and an `f32` as a `double`.
- **Each backend uses the target's variadic convention.** LLVM declares the function `(…, ...)`
  and places the arguments itself. Cranelift has no variadic calls, so each call is an ordinary
  one whose signature puts the arguments where the convention wants them
  (`crates/velt_codegen_cl/src/function/variadic.rs`), as rustc_codegen_cranelift does: on Apple
  arm64 zeros fill the argument registers so each variadic argument lands in its stack slot; on
  Windows a variadic `double` passes its bits as an integer. SysV x86-64 also wants `%al` to
  bound the vector registers used, which Cranelift cannot set, so a debug build (and `velt dev`)
  on x86-64 Linux or macOS refuses a call with a float variadic argument, naming `--release` as
  the way.
- **Header check.** A declaration with `CVarArgs` matches a variadic prototype and one without it
  a fixed one; each mismatch is an error with the fix.

### One C function, several declarations

One C function is often declared more than once in a program (velt/ffi declares
`memcpy(dst: u8[], src: Ptr<u8>, n: usize): Ptr<u8>`, the unwinder
`memcpy(dst: Ptr<u8>, src: StrBytes, n: usize): Ptr<u8>`, the scheduler
`memcpy$sched(dst: Ptr<u8>, src: Ptr<u8>, n: usize): Ptr<u8>`). They link one symbol, so they
must pass the same C signature, but they need not name the same Velt types.

- **Same C signature** is decided by class, the same on every target: every pointer form and
  `isize`/`usize` are one class (a machine word); each other integer is its own class by width
  and signedness; `bool`, `f32`, `f64` and `void` are classes of their own. Two declarations
  agree when they have as many fixed parameters, both or neither are variadic, and each position
  is of one class.
- **Lowering**: the symbol's first call fixes the extern's VIR signature, and a call through
  another declaration of the same C signature casts each argument and the result whose VIR type
  differs, so no backend sees two signatures.
- **A real mismatch is an error** at each declaration that differs, naming the other one and the
  first position that differs.
- This rule is about two declarations agreeing with each other, which the linker cannot see; the
  header check is what says either is right.

### A position in an array

C often wants the address of an element: `recv(fd, buf + filled, room)`. In a `declare c` call,
`xs.subarray(begin, end)` of a number array passed for a `T[]` or `readonly T[]` parameter
passes the address of element `begin`, clamped as JavaScript's `subarray` clamps, with no object
made and nothing copied. Lending follows the whole array's rule.

```ts
declare c("c") function recv(fd: i32, buf: u8[], n: usize, flags: i32): isize;

// SAFETY: `data` has `end` bytes; recv writes at most `end - filled` of them.
const got = unsafe { recv(fd, data.subarray(filled, end), (end - filled) as usize, 0) };
```

An unsafe `addressOf(xs, i): Ptr<T>` is deliberately absent: it would hand Velt code a raw
address into an array that a later `push` may move.

### C structs

```ts
declare c struct CGPoint { x: f64; y: f64; }
declare c struct CGSize { width: f64; height: f64; }
declare c struct CGRect { origin: CGPoint; size: CGSize; }
```

- A C struct has fields only: no methods, constructor, type parameters, defaults, optional,
  private or static fields, `implements`, and at least one field. Fields are integers, `f32`,
  `f64`, `bool`, C pointers (`Ptr<T>`, `Ptr<T> | null`: one word each, NULL as `null`; `Ptr<T> |
  undefined` is the same word, NULL as `undefined`, as in signatures) and other C structs; a struct cannot hold itself by value (a `Ptr<S> | null` field points at one). A
  Velt enum, a `declare c type` by value and `Ptr<T> | null | undefined` (two words) are refused
  with what to declare instead.
- In Velt it is a value: Copy, built from a literal of its type, with read-only fields (a
  changed copy is `{ ...v, x: … }`). Lowering never counts or boxes it, so it stays inline, in
  declaration order at natural alignment, which is C's layout for these fields on 64-bit
  targets. Lowering checks this for every C struct, from the fields' C types apart from the
  aggregate (`lower/c_layout.rs`, an `ICE:` otherwise).
- **`Ptr.read<S>(p, index)` and `Ptr.write<S>(p, index, s)`** (unsafe) move the `S` at element
  `index` of `p` to and from C memory, `index` times `S`'s size past its address. `S` is a
  number, `bool`, a pointer or a C struct; `void` and a `declare c type` have no bytes Velt
  knows and are refused, at the call or where a type parameter is instantiated. A read checks
  every pointer in the value declared non-null, in nested structs too: one C left NULL stops the
  program, naming the field (`inner.data`). A struct without such a field pays nothing. A `bool`
  read is its byte's truth to C, 0 or not, so a byte holding 2 is `true` (and `=== true`).
- On wasm32 C's pointers, `size_t` and `ssize_t` are 4 bytes and Velt's 8, so `Ptr.read` and
  `Ptr.write` of a type holding one are compile errors naming the target; a struct of other
  fields is C's layout there too.
- It has no identity, so every comparison compares its content, with the numbers in it compared
  as the operation compares numbers: `===`, `==` and `indexOf` by IEEE, `Object.is` by
  SameValue, `Map` and `Set` keys and `includes` by SameValueZero.
- `Ptr<S>` is a C pointer to it. A C struct is not raw API: a package's users can import it.

### Constants from headers

`declare c const NAME: T;` declares a constant whose value the headers give per target. `T` is an
integer, `f32`, `f64`, `bool`, `Ptr<U>` or `Ptr<U> | null`.

- It has no library in its syntax: the package's libraries that list `headers` are read in turn,
  and two that give different values are an error.
- clang evaluates what C can: an object-like macro, an enumerator, a `static const`. A
  function-like macro or a name the headers lack is "not defined"; a variable or a string "has no
  value a program can use".
- For integers the value decides: any integer type that holds it fits (a flag `#define`d as
  `int` can be a `u32`). A float type takes its C type's, `bool` `_Bool`, a `Ptr` a pointer
  (NULL only if `| null`).
- A function pointer constant is declared `Ptr<void>` (`SQLITE_TRANSIENT`), and a function
  pointer parameter likewise.
- Reading one needs no `unsafe`: the value is what the headers say. On a target without C
  headers (WebAssembly) a reachable read is an error.

### Exported functions: `@export("c_name")`

A top-level Velt function that C calls by name or through its address (the Velt scheduler's
worker threads start on one):

```ts
import * as ffi from "velt/ffi";

@ffi.export("tiny_on_row")
function onRow(n: i32, row: Ptr<u8> | null): i32 { … }
```

- **The decorator** is velt/ffi's (`std/ffi/export.velt`), found by its hook role
  (`decorator_export`), not by name. `export` is a keyword, so a module writes `@ffi.export(…)`
  through a namespace import or renames it (`import { export as cExport } from "velt/ffi"`).
- **Who may use it**: it is raw native API. An export defines a C symbol, so it could replace a C
  library function for the whole program (an exported `getaddrinfo` takes over name lookups), so
  only std and allowed native packages may import it.
- **What may be exported**: a top-level function with a body that is not generic, async, a
  generator, a method or nested. Its parameters and result are what a `declare c` function
  passes by value: integers, `f32`, `f64`, `bool`, `Ptr<T>`, `Ptr<T> | null`, and `void` as the
  result. It throws nothing: C has no exceptions.
- **No module state**: C may call it on a thread Velt did not start, which has no task to give
  the module lock back at the end of a step. So module state (module-level `let`s and kept
  constants) is not reachable from an export, directly or through what it calls; the decorator
  reports the path.
- **The name** is a C identifier of at most 255 bytes, given to one function of the program; it
  cannot start with `velt_` (the runtime's) or `rust_`, be reserved by C (`__…`, `_` and a
  capital), be `main`, a C keyword or one of the C library's common functions and objects, or
  collide with a `declare c` function of the program.
- **It compiles to** a function with the unmangled symbol and the target's C calling convention,
  which calls the Velt function, which stays an ordinary function for Velt callers. A pointer is
  the same word on both sides, so nothing converts; a `Ptr<T>` parameter C passes NULL for stops
  the program, naming the export, as it would read as `null`. It is built whether or not Velt code reaches it. In LLVM its `i8`, `i16` and
  `bool` parameters carry no `signext`/`zeroext`, since AAPCS64 outside Darwin leaves their upper
  bits unspecified; the export extends them itself.
- **Panics never unwind into C**: one that reaches the export ends the process. The function is
  `vir::Function::stops_unwind`. In LLVM every call in it is an `invoke` whose landing pad
  aborts, as clang does for `noexcept` and Rust for `extern "C"` (`landingpad … cleanup` with
  `rust_eh_personality` on Linux and macOS, a `cleanuppad` under `__C_specific_handler` on
  Windows); an `invoke` is never a tail call, so the frame always exists to stop at. Cranelift on
  Linux and macOS emits no unwind information for it, so the unwinder cannot pass it; Cranelift
  on Windows x64 names a language handler (`velt_eh_stop`, `crates/velt_codegen_cl/src/eh_seh.rs`)
  that ends the process when a Velt panic's search phase reaches the frame
  ([unwinding.md](unwinding.md)).
- **Async code in it**: on a thread that isn't one of the executor's workers (a C
  library's), the promises it starts, its timers and sleeps run on the executor's worker 0, handed
  over through its inbox (std/lang/hooks/sched_foreign.velt). The C function marks the call's start
  and end (`schedExportEnter`, `schedExportExit`), and such a thread's tasks wait for the
  outermost call's end, in the order they were made: the call's synchronous run comes first, as a
  JS callback's comes before its microtasks, so a rejection it handles later in that run is not
  reported meanwhile. A `spawn` there runs beside the call at once. Work handed over after `main`
  and the tasks left ended (a thread that outlives them) never runs: the process is exiting.
- **Its address**: `exportedAddress(f)` (std only, unsafe) is the C function's address as a
  `usize`, for C APIs that take a function pointer (`pthread_create`, `qsort`); its
  argument must name an `@export` function.
- **Linking**: a package's C `sources` call an export by name. Release builds link the static
  archive; debug builds link the shared library, which finds the symbol in the executable when
  it loads (macOS links the library with `-undefined dynamic_lookup`). Release links strip an
  export nothing references.
- **WebAssembly**: using the decorator is an error there.

### Checking declarations against C headers

Every declaration of a library that has headers is checked against them, so a wrong `int` for
`size_t` is a build error, not memory corruption.

```ts
native: {
  c: {
    c: { headers: ["<string.h>"] },
    sqlite3: { sources: ["native/sqlite3.c"], headers: ["native/sqlite3.h"] },
  },
  targets: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"],
}
```

- **The parser** is clang from the pinned zig (`crates/velt_toolchain/src/zig.rs`, version
  0.17.1, downloaded with its checksum into `$VELT_HOME/cache/zig/` on first use, or `$VELT_ZIG`).
  `zig cc -###` gives the clang command for the target, which runs with `-ast-dump=json`
  **once per target**, since types differ by target (`long` is 32 bits on Windows). `<name.h>`
  means a header of zig's set, the C library's for each target; a library's own headers ship
  with the package (`native/sqlite3.h`). Windows targets read zig's MinGW headers, which have
  MSVC's type sizes.
- **Functions** must exist in the headers with as many parameters, variadic exactly when the
  declaration is, and each parameter and the result must match after typedefs resolve:

  | Velt | Matches |
  |---|---|
  | `i8`…`u64`, `f32`, `f64` | the C type of that width and signedness; a C enum by its underlying type |
  | `bool` | `_Bool` / `bool` only |
  | `isize`, `usize` | `ssize_t`, `ptrdiff_t`, `intptr_t` / `size_t`, `uintptr_t`, which require them, so a declaration right on one target is right on all |
  | `Ptr<T>`, `Out<T>` | `T *`, `T` the C type of that name (`Out<Ptr<T> \| null>` is `T **`) |
  | read-only arrays, `StrBytes`, `CString` | `const T *` (a non-const `T *` is accepted: many APIs omit `const`) |
  | written arrays, `Out<T>` | non-const `T *` only |
  | `Ptr<void>`, `null` | `void *` or a function pointer (`null`: any pointer) |

  A function whose type has a calling convention other than the target's C one (`vectorcall`,
  `preserve_most`, `ms_abi` off Windows) is refused. An enum whose width clang's dump does not
  show cannot cross.
- **A mismatch is a build error** naming the declaration and the header line.
- **Remembered**: a check that passed is kept in `$VELT_HOME/cache/headers/`, keyed by zig's
  version, the target, the headers' names, the library's `defines` and the declarations, with a
  digest of every file the parse read, so clang runs again only when one changes. The package's
  files among them are build-stamp inputs, so a changed header rebuilds the program.
- **Installs check what was checked.** `velt native headers` checks a package on every target
  with C headers (macOS, Linux gnu and musl, Windows; x86_64 and aarch64) and writes
  `c-headers.json` at the package root: per target and library, a digest of each declaration, of
  each of the package's own headers the parse read, and of the library's `defines`, plus the
  values of its `declare c const`s. `velt publish` runs it for a package with `native.c`; git
  packages commit the file. A fetched package's declarations must be in the record for the
  build's target, and each recorded header it ships must have its digest. Users never run clang.
- **Every C library a package declares functions of lists `headers` or says `unchecked: true`.**
  `velt check` and the `allowNative` error show which libraries are unchecked.
- **std** has no manifest, so `velt native headers --std` checks the std files listed in
  `crates/veltc/src/native/headers/std_check.rs` against the C library headers listed there, on
  every target; CI's `c-headers` job runs it (`crates/xtask/src/c_headers.rs`). The list only
  grows.

The record is not a signature: it catches mistakes, not a hostile package, which is what
`allowNative` is for. What stays trusted is what no header expresses (what a returned pointer
points to and how long it lives, raw reads), and it lives in the `unsafe` blocks with their
stated reasons.

### The trust boundary

- **`allowNative`.** Installing or building with a package that has native code (C libraries in
  `native.c`, or a Rust crate) is an error until the root's `package.velt` lists it. An entry
  binds to a source: a bare name allows the registry and local paths, `{ name, git }` that
  repository at any commit, `{ name, url }` archives under an https prefix
  ([manifest.md](../contracts/manifest.md)). The check runs after resolution, on the source that
  won. The root's `allowNative` is recorded in `velt.lock.json`, so a change of trust shows in
  the lockfile's diff.
- **A package declaring C functions has `native.c`** (`c: {}` when it calls only the C library);
  without it, its first declaration is an error. So its manifest says it has native code.
- **`unsafe` code** is allowed only in std and in allowed packages with native code (the program's
  own files too, when its own `package.velt` declares native code). veltc checks the loaded
  modules' ASTs (`crates/veltc/src/native/trust.rs`); sema has already required an unsafe context
  for each unsafe call, so confining the contexts confines the calls. A file under
  `node_modules`, or under a directory with its own `package.json` or `package.velt`, has no
  package's trust.
- **The raw API stays among native packages.** `Ptr`, the `export` decorator,
  `declare c type`s and unsafe functions can be imported only there: by name, re-exported, or as
  a namespace member. A safe function of an allowed package can still return a `Ptr`; the code
  receiving it cannot read through it without `unsafe`.
- **What the AST check cannot see stays out of reach another way.** Dynamic dispatch (`any`,
  `Proxy`, `Reflect`) does not reach unsafe methods; the methods the language calls implicitly
  (symbol-keyed ones, `toString`, `valueOf`, `toJSON`, `then`) cannot be unsafe; generated
  source cannot contain `unsafe` code.
- **`velt check`** ends, when it passes, with each allowed package with native code: its C
  libraries (the system library each links on the target, or the sources it builds, and whether
  its headers were checked), whether it has a Rust crate, and each piece of its unsafe code by
  file and line. `--json` has the same under `native`.

### Where libraries come from

Every library is a logical name in `native.c`, with one source per target:

- **`system`**: maps build conditions (`native`, `darwin`, `linux`, `win32`, `default`) to a
  library name or, under `darwin`, `framework:Name`; the first condition the build has applies.
  The name is checked to be at most 64 bytes and never a linker flag or a file. Every build links
  the libraries the program's declarations name (`-lname`, `-framework Name`, `name.lib`), and
  `velt dev` opens them (`dlopen` with `RTLD_GLOBAL`, `LoadLibraryA`). The bundled linker's kits
  have only the platform's own libraries, so another system library links with the system
  linker.
- **`sources`**: `.c` files under `native/` with optional `defines`, built by `velt native build`
  with the pinned `zig cc` for any target from any machine (macOS 11 on arm64 and 10.12 on
  x86_64, glibc 2.17). The build is reproducible: `-O2 -g0 -fPIC -ffunction-sections`, run in the
  package root with relative paths and `SOURCE_DATE_EPOCH=0`, so two builds in other directories
  give the same bytes. A source may read only `native/` and zig's own headers; anything else is
  refused, since the package would not ship it. A bundle is reused while a hash of its inputs
  (zig's version, flags, target, package, sources, defines and every file under `native/`) is
  unchanged. Libraries are built when a program links or loads them
  (`crates/veltc/src/native/c_sources.rs`), never at install. `velt publish` builds every
  `targets` entry, and installs fetch and verify the bundles like a Rust crate's, so users never
  run zig. Building from source instead needs `VELT_NATIVE_FROM_SOURCE=1`.
- **Linking**: debug builds link the shared library (with an rpath), release builds the static
  archive, and musl programs always the static one. Windows links the DLL in both modes: zig's
  Windows objects are MinGW's, and a DLL brings its own C runtime behind its import library.
  `velt dev` loads the shared library with `RTLD_GLOBAL`; a change under `native/` restarts the
  host.
- **`c`, the C library**, is predefined: libc, libSystem, or the UCRT on Windows; WASI's libc on
  `wasm32-wasip1`, the only library there ([wasi-libc.md](wasi-libc.md)). Since Visual Studio
  2015 the UCRT defines `printf`, `snprintf` and the rest of the stdio family inline in
  `<stdio.h>`, and `ucrt.lib` exports none of them, so a program that declares a function from
  `c` and targets `*-windows-msvc` also links `legacy_stdio_definitions.lib`
  (`crates/veltc/src/native/c_decls.rs`). A static library adds only what a program calls, so
  this costs nothing when unused.

`packages/sqlite` builds SQLite's amalgamation from `native/` with the options rusqlite's
`bundled` build uses as `defines`, and declares its `SQLITE_*` constants with `declare c const`.

### std's system calls

std reaches the operating system through `declare c("c")` like any package: the file-descriptor
API (`std/fs/fd/`, chosen per target by the `#fd-system` conditional import), sockets and the
reactors, process and OS queries. The guards a system call needs live in Velt beside each unsafe
call, with their `SAFETY` comments: a path ends with its only 0 byte (`checkPath`), a buffer
range is clamped to the array and C gets the clamped length, `fcntl` takes only commands whose
argument is an `int` (`fcntlInt`). Each guard panics, since a failing one is a bug in std, never
bad input (`std/fs/fd/guards.velt`). Targets without the POSIX file API (Windows, WebAssembly)
get `std/fs/fd/unsupported.velt`, whose functions fail with `ENOSYS`.

### Per-platform backends

- **Same C API, different library**: one logical library mapped per target in `system` (CBLAS on
  Accelerate, OpenBLAS or MKL), one set of declarations.
- **Different APIs**: conditional imports. Each variant declares its own library's functions and
  exports the same names and signatures:
  `"#backend": { darwin: "./src/backend/accelerate.velt", linux: "./src/backend/openblas.velt" }`.
  Only the chosen variant's libraries link.

### C++

C++ libraries are reached through their C API (llama.cpp, ONNX Runtime, LLVM-C and wgpu-native
all have one). A C++-only library gets a small `extern "C"` wrapper that catches every
exception; one escaping into Velt is undefined behavior. Direct C++ interop (mangled names,
classes, templates) is a non-goal.

## Compilation

- **Syntax** (`crates/velt_syntax/src/parser/c_decls.rs`): `declare c` forms, `unsafe` blocks,
  expressions and modifiers.
- **Sema** (`crates/velt_sema`): C signatures (`collect/c_sig.rs`), C structs
  (`collect/c_struct.rs`), unsafe contexts (`body/unsafe_code.rs`), variadic arguments
  (`body/expr/c_varargs.rs`), the `@export` rules (`decorators/export.rs`).
- **Driver** (`crates/veltc/src/native/`): the trust boundary (`trust.rs`, `unsafe_sites.rs`),
  the header check and constants (`headers/`), libraries per target (`c_decls.rs`), C sources
  (`c_sources.rs`). The driver sets each `declare c const`'s value in HIR (`GlobalDef::c_const`)
  after sema.
- **VIR** (`crates/velt_vir`): a C call is a direct call of a `vir::ExternFn`; lowering
  (`lower/c_call.rs`, `lower/extern_args.rs`, `lower/system_views.rs`) takes addresses of
  borrowed arrays and strings, makes `Out` slots, casts between declarations of one C signature,
  promotes variadic arguments and stores `errno`.
- **Codegen**: a direct call with the target's C convention in both backends.
- **Link** (`crates/velt_link`): the libraries' archives or shared libraries and the system
  libraries.

## Runtime cost

- A `declare c` call is a direct C call. `StrBytes`, borrowed arrays and `subarray` arguments
  cost nothing beyond passing an address; an `Out<T>` is a slot the call writes. A pointer is
  one word in a register, with no conversion.
- `errno: true` adds a call to the C library's `errno` accessor and a store after each call.
- A `declare c const` is a constant compiled in; a C variable is a load or store per use.
- At compile time, the header check runs clang once per library and target when its inputs
  change (about 0.2 s per library), and is remembered otherwise; installs compare digests only.

## Diagnostics

Representative messages (notes after the first line):

```text
error: calling unsafe function `sqlite3_step` needs an `unsafe` block or function
  note: wrap the call in `unsafe { … }`, with a `// SAFETY:` comment before it saying why the
        call is sound
error: using C variable `environ` needs an `unsafe` block or function
error: `unsafe` code needs a native package
  note: only std and packages with native code that the program allows (`allowNative`) may
        use it
error: `unsafe` code needs a `// SAFETY:` comment directly before it
error: `Ptr` of `velt/ffi` is raw native API
error: package `llama` (through `chat-kit` → `llama`) has native code, which runs with no
       safety net
       allow it in package.velt, if you trust it: allowNative: ["llama"]
error: `string` cannot be passed to C: declare parameter `v` of `f` as `StrBytes` (velt/ffi)
       to pass its bytes, borrowed
error: C function `sqlite3_bindtext` is not declared in its library's headers (…)
error: parameter 4 of C function `sqlite3_bind_text` is `int` in sqlite3.h:…, not the
       declaration's `i64`
error: C function `abs` has two signatures: …
error: `sqlite3` has no library on wasm32-wasip1: only the C library `c` (wasi-libc) is
       linked there
```

## Alternatives considered

**A signature table in the compiler** for the C library. Safe, but closed to every other
library, and the guards it enforced are clearer as Velt code beside each unsafe call.

**A run-time marshalling layer** (P/Invoke's default). Conversions on every call, invisible in
the source. Here a `string` crosses only as `StrBytes` or a `CString` the binding made, so no
call allocates on the common path.

**`CVarArg` values made by a constructor** (Swift's `withVaList` route). An allocation per
argument for what is a register move, and nothing checked that the argument's own type does not
already say.

**Requiring identical Velt types for two declarations of one C function.** std and packages could
not be loaded together when they spell one C function differently though each is right. Merging
by VIR type alone fails too: `usize` is `u64` natively but `u32` on wasm32.

**`declare c("lib") var`, or an address-only form for C variables.** Velt has no `var`, and an
address-only form would make every use read through `addressLoad` by hand. Giving
`declare c const` a run-time meaning when a library is named would let one word decide between a
compile-time and a run-time value.

**Capturing `errno` when the program reads `lastOsError()` anywhere.** What a `declare c` call does
must not depend on code elsewhere.

## Limitations

- `declare c` calls are synchronous.
- A C struct crosses only behind `Ptr<S>` (read and written with `Ptr.read` and `Ptr.write`):
  not by value, not as `Out<S>`; struct declarations are not compared with headers. C arrays in
  structs are not supported.
- Function pointers cross only as `Ptr<void>`; C cannot call a Velt closure.
- `declare c const` cannot be a string; a macro of a string literal is an error.
- `va_list` parameters (`vprintf`) cannot be declared.
- `sources` are C only. Frameworks cannot be built from `sources`, and header lists are not
  per build condition, since zig has no macOS SDK.
- On Windows, an export cannot be called by name from a package's C sources (the DLL cannot call
  into the executable), and `velt dev` cannot find the out-of-line stdio functions. Under
  `velt dev`, an export cannot be called by name (its address works).
- Registry packages without C `sources` do not ship their headers, so installs trust that the
  author ran the check on the headers they published.
- On WebAssembly only the library `c` exists.

## Not yet implemented

- **C structs by value** in `declare c` signatures, `Out<S>` and `Ref<S>`, following the target's
  convention (SysV x86-64 eightbytes, AAPCS64 homogeneous float aggregates, Win64 by reference
  over 8 bytes, `sret`). One table-driven classifier, a port of rustc's
  `rustc_target::abi::call`, serves both backends, and a differential ABI test (generated C
  functions over many struct shapes, compiled with the system C compiler, called from both
  backends on every CI target) gates it. Structs are compared with headers (names, order, types,
  size, alignment, offsets per target), and C arrays become fields once Velt has fixed-size
  arrays.
- **`NativeHandle<T>`**, after .NET's `SafeHandle`: a native object's pointer with its release,
  which runs exactly once. It counts calls in flight: passing it for a `Ptr<T>` parameter takes
  a reference for the call, `dispose()` marks it closed, the release runs when the last call
  ends, and using a closed handle throws `HandleClosed`, never a use-after-free.
- **`NativeCallback<F>` and `CFunction<F>`**: a Velt closure handed to C as a trampoline and a
  `CallbackContext`. Captures follow `spawn`'s transfer rules. A callback runs at once when its
  owning task is inside a C call on that thread (`qsort`, `sqlite3_exec`) and is queued into a
  fixed, lock-free ring otherwise (C's own threads), run when the owner is next polled; only
  `void` callbacks may be queued. `CFunction<F>` replaces `Ptr<void>` for function pointers and
  is compared with headers recursively. Errors never unwind into C.
- **`PinnedBuffer<T>` and leases**: memory outside the counted heap that never moves, lent to
  native code past one call. A lease for a synchronous call is a borrow the ownership checker
  sees; one moved into an async call keeps the memory until C is done, whatever happens to the
  task.
- **`declare c async`**, cancellation-safe by one rule: what C keeps is moved into the call and
  released when C is done. A blocking call runs on the library's own thread pool (sized in the
  manifest, overridable by the root and at run time). A non-blocking call takes a
  `Completion<R>` that native code completes once from any thread through a small public C
  header, `velt.h` (`velt_complete`, `velt_fail`, `velt_cancelled`, `velt_alloc`).
- **Rust crates through `declare c`**: a `crate` library kind, built with cargo, exporting
  `extern "C"` functions checked against a header generated from the crate; `velt.h` exposing
  Velt's reactor (socket readiness, wakers, spawning, timers) so crates generic over the standard
  async I/O traits run on Velt's scheduler without thread hops. The Rust-specific native ABI of
  [native-packages.md](native-packages.md) is then retired.
- **C++ and Zig sources**, a system library a source needs, and macOS frameworks built from
  sources with a publisher-supplied SDK (`--macos-sdk`).
- **Fetched sources**: `sources` read from a git repository or a URL pinned by hash.
- **Rebuild verification**: the registry or CI rebuilding a bundle from `sources` and comparing
  checksums, which the reproducible build makes possible.
- **A main-thread task** driven by the platform's loop, for GUI bindings.
- **Bindings from headers**: a source generator that reads clang's AST and writes declarations
  that pass the check by construction.
