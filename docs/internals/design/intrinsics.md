# Intrinsics

Status: partly implemented. Issue: [#1981](https://github.com/velt-lang/velt/issues/1981).

## Summary

An intrinsic is an operation whose meaning the compiler knows instead of a function body it
compiles: `Math.sqrt`, `x.countOnes()`, a SIMD lane add, an atomic compare-and-swap, a SHA-256
round. Knowing it is what lets the compiler emit one instruction, fold it on constants, move it
out of a loop, or pick the instruction a CPU has.

Each intrinsic family has one registry in `velt_vir` that names its operations, their signatures,
memory effects and constant evaluation. Backends, optimizer passes and the VIR verifier match on
those enums, never on symbol strings, and pure operations are VIR statements rather than calls.

## Motivation

When every intrinsic was lowered to the call of an extern whose name each consumer matched on its
own, the lists drifted (a pass missed functions LLVM knew to be pure), every `sqrt` ended a basic
block, every pass that did not look the name up saw a call that may write any memory, and nothing
was folded on constants. One registry per family removes the drift, and the statement form lets
the generic passes treat `sqrt(x)` like `x * x`.

## Design

### Declaration

`std/lang/intrinsics/*.velt` declares each intrinsic (`@intrinsic declare function`), and
`crates/velt_sema/src/intrinsic_decls.rs` is the one table from name to `hir::Intrinsic`; a test
fails on a missing or an extra entry ([std-layout.md](std-layout.md)). Programs never name these
declarations: they use `Math`, `velt/simd`, `Atomic` and the integer methods.

Not every operation that compiles to one instruction is an intrinsic. `x.rotateLeft(n)` and
`x.swapBytes()` are plain shifts and masks in `std/prelude/bits.velt` that LLVM recognizes, and
the checked, saturating and wrapping arithmetic of the integer types is prelude code too.

### Families

| Family | Module | Operations | Runtime function behind it |
|---|---|---|---|
| math | `velt_vir::math` (`MathOp`) | sqrt, floor, ceil, JS round, trunc, abs, integer pow, f64/f32/f16/bf16 bits and conversions, umulh, ToInt32 and the int32 helpers, clz/ctz/popcount, fused multiply-add | velt_rt's `math.rs` for most; `velt_math_*` operations have none |
| simd | `velt_vir::simd` (`SimdCall`) | lane operations, loads and stores, masks, shuffles, the 256-bit `f32x8` | none: always inline |
| arch | `velt_vir::simd` (`ExtOp`, `Ext`) | SHA, carry-less multiply, AES rounds | none: behind a CPU check |
| atomic | `velt_vir::atomic` (`AtomicCall`) | loads, stores, read-modify-writes, CAS, fences | none: always inline |
| tls | `velt_vir::tls` (`TlsCall`) | thread-local reads and writes | none |

`velt_vir::math::effects` is the one list of what a runtime function may do to memory (anything,
read-only, read-caching, none, cold); LLVM's declarations and `velt_opt` both read it.

### Pure intrinsics are statements

`Rvalue::Intrinsic(Intrinsic, operands)` (`crates/velt_vir/src/vir.rs`, invariant 12) is an
operation that reads only its operands, touches no memory, and never panics or unwinds. Passes
treat it like arithmetic: it does not end a block, `constfold` evaluates it, copy propagation and
dead-code elimination see through it, and it may move out of a loop.

`Intrinsic::Math(op)` means exactly the call of `op`'s runtime function. The program declares
that function, and a backend calls it where it has no inline code; a `velt_math_*` operation
(`MathOp::has_runtime_function` false) has no runtime function and every backend emits it
inline. A math operation is never a direct call of its runtime function (the verifier checks
it). Operations with effects (atomics, loads, stores) stay calls of their externs, because their
order matters.

### The runtime function is the reference

Where an operation has a runtime function, that function defines what it computes. `MathOp::eval`
folds constants and refuses where folding could differ from running: a NaN argument or result of
a double operation, whose bits the instruction picks. A veltc test
(`crates/veltc/src/math_fold_tests.rs`) compares `eval` with the runtime's own functions, linked
through `velt_rt_host`, on edge values and pseudo-random inputs. For a `velt_math_*` operation
`eval` is the reference; the f16 and bf16 conversions share theirs with sema's literal parsing
(`velt_common::half`).

### Portable and CPU-specific

The line that matters is portable against CPU-specific, as in .NET (`Vector128` against
`Sse2`/`AdvSimd`) and Rust (`std::simd` against `core::arch`):

- **Portable** (math, simd, atomic): the same meaning on every target; every backend implements
  it.
- **CPU-specific** (`Ext`: SHA, PCLMULQDQ/PMULL, AES, x86 FMA): one architecture's instruction,
  reached only behind a check of its extension or in a function gated on it (`targetFeature`,
  [rt_abi.md](../contracts/rt_abi.md) "SIMD"); a backend without it never reaches it. A check
  this build answers false (the other architecture's, or any under Cranelift,
  `LowerOptions::no_cpu_extensions`) is known in lowering
  (`crates/velt_vir/src/lower/cpu_checks.rs`), so what only it reaches is never lowered. A check
  inside a function gated on the extension, or in code built with `--cpu` for a CPU whose
  start-up-checked features include the extension (`velt_codegen_cl::ext_x86_features`), is the
  constant `true`.
- **Language runtime** (generators, channels, `any`, reflection, weak references, ownership
  facts): operations of the language, not of the hardware. They stay `hir::Intrinsic`s that
  lowering expands.

`--cpu` gives a compile-time mask over `velt_codegen_cl::X86_FEATURES` on x86-64 and
`ARM_FEATURES` on arm64, checked at start-up by std's `cpuStart` hook
([rt_abi.md](../contracts/rt_abi.md) "CPU check"). The run-time answers of the extension checks
(velt/arch's `isSupported()`, package-machine-code.md §2 "As built in step 3") come from the
feature word that hook computes once.

### Memory at an address

std's runtime code written in Velt (the async executor, channels, the FFI) needs to read and
write raw memory, change a word atomically where C or the OS can see it, and call function
pointers. These are std-only intrinsics (`std/lang/intrinsics/address.velt`, `atomic.velt`,
`future.velt`); programs and packages reach none of them directly. The ones that touch raw memory
are `@intrinsic declare unsafe function`, so their callers are unsafe code with a `// SAFETY:`
comment. velt/ffi's unsafe functions are the public surface ([c-ffi.md](c-ffi.md)).

- `addressLoad<T>(address)` and `addressStore<T>(address, value)` read and write one `T` at an
  address: one VIR assignment from or to `*(T*)address`, so every backend emits a plain load or
  store. Sema holds `T` to a number type or `bool` and refuses a type parameter, since an
  instance with a class would move an owned reference as raw bytes.
- `addressArray<T>(address, length): T[]` is an array over memory it does not own: the header
  `{ data: address, len: length, cap: 0 }`, the form a constant table's read-only data has, so
  dropping it frees nothing and a copy of it is an ordinary array. `T` is a number type or `bool`.
  Every read is the usual bounds-checked element read or SIMD load, at no cost over an owned
  array. Nothing may write or grow it, so std wraps it in `shared(...)` before it leaves the
  unsafe function that makes it: `shared<T[]>` is read-only for every holder. Its users are
  views that live no longer than their memory (`std/fs/fd/dirs.velt`,
  `std/process/stdin_buffer.velt`); `MappedFile` uses the form with an owner below.
- `addressArrayOwned<T>(address, length, owner): T[]` is that array with an owner that is told
  when the array is dropped. `owner` is the address of a block whose first word is a C function
  taking the block; the header is `{ data: address, len: length, cap: owner | 1 << 63 }`, and
  dropping the array calls the function instead of freeing a buffer (one test of `cap`'s top
  bit where a buffer is freed; a capacity is never that large). The block is the owner's own:
  it counts the arrays it gave out and gives the memory back with the last (`munmap`, a C
  library's `free`). `MappedFile`'s is in `std/fs/mapped_part.velt`: the function, the count,
  and the mapping's address and length; `close()` drops the `MappedFile`'s own array. A copy of
  the array is an ordinary array with no owner. As the array lives in a `shared<T[]>`, its one
  drop is the box's, on whichever thread drops the last handle, so the owner's function must be
  callable from any thread.
- `arrayDataAddress<T>(xs: Mut<T[]>): usize` and `arrayReadAddress<T>(xs: readonly T[]): usize` (both
  unsafe), `arrayByteLength<T>(xs: T[]): usize` and `arrayBytesWritten`
  (std/lang/intrinsics/array.velt) are the
  address of a number array's buffer and `xs.length * sizeof(T)`. They exist so that a generic
  function can hand a number array's own memory to a C function, which `declare c` takes as a
  buffer only for a concrete element type: velt/fs's `readSync<T extends Numeric>` and
  `writeSync` pass the address to `pread` / `pwrite` (`readAddress`, `writeAddress` of
  `#fd-system`), so the system fills or reads the array itself, counted in bytes as Node counts
  a typed array's. The address is good only until the array is grown or dropped: std takes it
  after every argument is checked and uses it in the read or write that follows, with nothing
  in between that reaches the array.
  - `arrayDataAddress` is for memory that will be written: it counts as a change of the array
    (the owner and write barriers of array/ops.rs), so `readSync` into an array an `RwLock`
    holds stops in a `read` callback as any other change does. `arrayReadAddress` and
    `arrayByteLength` only read, as `length` does, so `writeSync` of such an array works there.
  - Sema holds `T` to a number type or `bool` and lets a type parameter through (as
    `arrayCopyBytes`); lowering refuses an instance whose elements own something, which the
    `Numeric` bound of the public functions keeps user code from reaching.
  - On a view type (array_view.rs: a number array type is one in a program that converts an
    `any` to it) a plain array gives its own buffer, and a view its mirror's with every element
    converted, which is what a C function given the array itself gets. Taking the address to
    write (`arrayDataAddress`) keeps a copy of a view's mirror, and
    `arrayBytesWritten<T>(xs: Mut<T[]>)`, which `readSync` calls after the read, writes back to
    the viewed array the elements that differ from that copy: what the read changed, and
    nothing else, so an element the view's type rounds (`0.1` seen as an `f32`) keeps its own
    value where the read did not reach it. It is no code for a plain array or in a program
    without view types. A C function given a view as a buffer that is not `readonly` gets the
    same keep before the call and write-back after it (c_call.rs).
  - The bytes are reached by address, not as a second `u8[]` over the same memory (the form
    `addressArray` gives): such an array would be a counted box in a program that shares byte
    arrays, an allocation and a free at every call, and an alias that must be kept from
    escaping. An address is neither.
- **`Numeric`** (std/prelude/number.velt) is that bound: an interface no type declares, which
  the compiler takes to be satisfied by the integer and float types (`f16` and `bf16` too) and
  by nothing else, as `AtomicValue` and `Frozen` are the compiler's (velt_sema `infer.rs`
  `satisfies`). A type parameter has it only through its own `T extends Numeric`; `bool` is
  left out (not every byte is a `bool`).
- `addressAtomicLoad<T>`, `addressAtomicStore<T>`, `addressAtomicRmw<T>`, `addressAtomicCas<T>`
  and `addressAtomicCasWeak<T>` do what `Atomic<T>`'s methods do on the integer `T` at an
  address, with the same `velt_atomic_*` operations (`crates/velt_vir/src/atomic.rs`). The
  orderings and `addressAtomicRmw`'s operation are string literal parameters; the declaration's
  union types reject an ordering the operation cannot have, and sema turns the literals into the
  integer literals the atomic intrinsics take (a value that is not a literal is an error). A type
  parameter is accepted here, since lowering checks each instance is an integer.
- `atomicAddress(a: Atomic<T>): usize` gives the address of an `Atomic`'s word for the OS's wait
  and wake calls (`futex`, `__ulock_wait`, `WaitOnAddress`). The address is useful only while the
  word stays where it is, so sema accepts two places, by the shape of the argument:
  - a *static atomic*: a module `const` made by `new Atomic<T>(literal)`, whose word is in the
    program's writable data for the whole run (`crates/velt_vir/src/lower/static_atomic.rs`);
  - a field (of a field…) of the value of a `shared` box read in place (`s.state`,
    `s.inner.waiters`) from a box that outlives the statement (a local, parameter, capture,
    global, or a field of one): a box never moves, and its value is immutable apart from its
    atomics.

  A local, an object field reached otherwise, an array element (an array may grow and move its
  elements), a module `let` or computed `const` (module state is per task), and a temporary box
  (`mk().n`) are errors. The address is valid while the caller holds a reference to the `shared`
  value. `T` is a 32- or 64-bit integer type, the widths the OS calls take. Getting the address is
  safe; using it is unsafe.
- `futPoll(f, cx)` and `futDrop(f)` call through a heap future's header
  ([rt_abi_async.md](../contracts/rt_abi_async.md) §1): the entry at offset 0 or 8 is loaded and
  called indirectly (`vir::Callee::Ptr`) with the C calling convention. Lowering's own polls and
  drops of heap futures (awaiting a promise value, `Promise.all`'s children, a `Promise<T>`'s drop
  glue) use the same code (`crates/velt_vir/src/lower/raw_memory.rs`). A header entry is an
  8-byte slot on every target (wasm32 keeps the table index in its low half), so the
  `{ poll, drop }` header is 16 bytes everywhere. These calls do not count as calls of an unknown
  function value in a `read` callback (`Boxing::read_indirect`).
- `statePoll(poll, state, cx)`, `stateDrop(drop, at)` and `addressCall(f, a)` call a compiled state
  machine's poll and drop functions and a waker table's entries at their addresses.
- `addressCallWord(f, a: u64)` and `addressCall3(f, a, b, c)` (three `usize`s) call the C
  function at address `f`, which returns nothing: the weak core's calls of retain, release and
  trace glue (`std/lang/hooks/weak*.velt`). Each is a call with the callee's own signature, which
  an indirect call on wasm32 must match (`crates/velt_vir/src/lower/raw_memory.rs`
  `address_call_void`).
- `fnAddress(f)` is the address of a module-level function C may call with the C calling
  convention (named directly, not generic, async or throwing, and taking and returning only
  numbers and `bool`, which sema checks). `exportedAddress(f)` is the address of a function
  decorated `@export("c_name")`, for a C API that takes a function pointer (`pthread_create`'s
  start routine, a callback).

## Compilation

### Adding an intrinsic

1. **Declare it** in `std/lang/intrinsics/<family>.velt` with a doc comment, or, for one that std
   reaches through a runtime function, `declare function velt_rt_<family>_<name>` in the std
   module that uses it.
2. **Register it** in its family's enum in `velt_vir` with its symbol and signature; give a
   runtime function its effects in `effects`.
3. **Define its reference**: the runtime function, or for an always-inline operation the
   semantics in the enum's doc comment (and `eval` for a `velt_math_*` one).
4. **Fold it** in `eval` where it is pure and the result does not depend on the hardware.
5. **Emit it** in both backends: velt_codegen_cl (`math_value` in
   `crates/velt_codegen_cl/src/function/terminator.rs`, or the family's module) and
   velt_codegen_llvm (`math_target` in `crates/velt_codegen_llvm/src/function/terminator.rs`, or
   the family's module). A CPU-specific one also gets its extension (`Ext`) and its check.
6. **Test it**: the Cranelift test programs (`crates/velt_codegen_cl/src/tests/programs/arith.rs`:
   JIT, object and linked runs), the LLVM native tests (compiled with clang, compared bit for
   bit), and for a folded one `math_fold_tests` against the runtime. Break the code on purpose
   and see the tests fail.
7. **Document it**: [rt_abi.md](../contracts/rt_abi.md) for its symbol, and the public API's
   docs page.

## Limitations

- CPU-specific operations are std-only. There is no public `velt/simd/x86` in the manner of
  `core::arch`; the gate and the check stay inside std until a package needs them.
- There is no general compile-time evaluation: folding is the optimizer's.
- The start-up check does not know AES, arm64 SHA and PMULL, FEAT_FHM, FEAT_BF16 or
  AVX512-BF16, and the x86 FMA extension is not mapped to its FMA bit, so `--cpu` never makes
  the checks of AES, x86 FMA, arm64 SHA or PMULL constant; code built for a CPU with FEAT_FHM or
  FEAT_BF16 stops with an illegal instruction on a CPU without them.
- `addressArray` is offered only over memory that never goes away; an array over memory that can
  be freed needs a lifetime the compiler tracks.

## Not yet implemented

- **A reference for every always-inline operation**, with a differential test of each through
  LLVM, Cranelift and wasm, and Cranelift calling the reference where it has no instruction.
- **One CPU-feature model**: every `Ext` mapped to a feature bit of the start-up check's lists on
  both architectures, so that each extension check is constant under a `--cpu` that has it.
- **Wider vectors**: 256- and 512-bit vector types beyond `f32x8`, split into 128-bit halves where
  the CPU has no wider registers, and saturating integer narrowing.
- **HIR**: `hir::Intrinsic` split into language operations and library intrinsics, and the
  builtins that still have hand-written signatures in sema (`panic`, `sleep`, `xs.push`) declared
  in std.
