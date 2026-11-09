# Design: intrinsics, operations the compiler knows

Status: **proposed** (issue #1981). Steps 1–3a are in pull requests (#1988, #1993, #1997); this
document describes the model they build and the steps after them.

## Problem

An intrinsic is an operation whose meaning the compiler knows instead of a function body it
compiles: `Math.sqrt`, `x.countOnes()`, a SIMD lane add, an atomic compare-and-swap, a
SHA-256 round. Knowing it is what lets the compiler emit one instruction, fold it on constants,
move it out of a loop, or pick the instruction a CPU has.

Velt's front end treats them that way. `std/lang/intrinsics/*.velt` declares each one
(`declare intrinsic function`), `velt_sema::intrinsic_decls` is the one table from name to
`hir::Intrinsic`, and a test fails on a missing or an extra one (std-layout.md §10). Below HIR
they were not: lowering turned every intrinsic into the call of an extern whose *name* each
consumer matched on its own.

- The scalar math functions (`velt_rt_math_*`, `velt_rt_pow_*`) were matched by string in
  velt_codegen_llvm (which LLVM intrinsic, which inline helper, which memory effects), in
  velt_codegen_cl (which instruction) and in three velt_opt passes. The lists had drifted:
  `borrowed_copies` missed two functions LLVM knew to be pure.
- A call ends its basic block, so every `sqrt` split a block, and every pass that did not look
  the name up saw an opaque call that may write any memory.
- `velt_opt` evaluated none of them on constants.

SIMD (`velt_vir::simd::SimdCall`) and atomics (`velt_vir::atomic::AtomicCall`) already had
one parser each, checked by the verifier and shared by both backends; that is the model this
design extends to every intrinsic.

## Model

**One registry per family, in `velt_vir`.** A family is an enum of its operations with each
one's symbol, VIR signature, memory effects and constant evaluation:

| Family | Module | Operations | Runtime function behind it |
|---|---|---|---|
| math | `velt_vir::math` (`MathOp`) | sqrt, floor, ceil, JS round, trunc, abs, pow, f64↔bits, umulh, ToInt32 and the int32 helpers, clz/ctz/popcount | yes: velt_rt's `math.rs`, the reference |
| simd | `velt_vir::simd` (`SimdCall`) | lane operations, loads and stores, masks, shuffles | none: always inline |
| arch | `velt_vir::simd` (`ExtOp`, `Ext`) | SHA, carry-less multiply | none: behind a CPU check |
| atomic | `velt_vir::atomic` (`AtomicCall`) | loads, stores, read-modify-writes, CAS, fences | none: always inline |
| tls | `velt_vir::tls` (`TlsCall`) | thread-local reads and writes | none |

Backends, passes and the verifier match on these enums, never on symbol strings.
`velt_vir::math::effects` is the one list of what any runtime function may do to memory
(none, read-only, read-caching, cold), which LLVM's declarations and `velt_opt` both read.

**Pure intrinsics are statements.** `Rvalue::Intrinsic(Intrinsic, operands)` (vir.rs invariant
12) is an operation that reads only its operands and touches no memory, so passes treat it
like arithmetic: it does not end a block, `constfold` evaluates it, copy propagation and
dead-code elimination see through it, and it may move out of a loop. `Intrinsic::Math(op)`
means exactly the call of `op`'s runtime function; the program declares that function, which
backends call where they have no inline code (`pow`, ToInt32's cold path). Operations with
effects (atomics, loads, stores) stay calls of their externs: their order matters.

**The runtime function is the reference.** Where a family has one, it defines what the
operation computes. `MathOp::eval`, which folds constants, refuses where folding could differ
from running (`pow` on doubles, which is fdlibm; a NaN, whose bits the hardware picks), and a
veltc test compares it with the runtime's own functions (linked through `velt_rt_host`) on
edge values and pseudo-random inputs.

**Grouped by kind, not only topic.** The line that matters is portable vs CPU-specific, as in
.NET (`Vector128` vs `Sse2`/`AdvSimd`) and Rust (`std::simd` vs `core::arch`):

- *Portable* (math, bits, mem, simd, atomic): the same meaning on every target, and every
  backend must implement it.
- *CPU-specific* (arch.x86, arch.arm): one architecture's instruction, only behind a check of
  its extension or in a function gated on it (`targetFeature`, rt_abi.md "SIMD"); a backend
  without it never reaches it. A check this build answers false (the other architecture's, or
  any under Cranelift, `LowerOptions::no_cpu_extensions`) is known in lowering, so what only it
  reaches is never lowered.
- *Language runtime* (generators, channels, `any`, reflection, weak references, ownership
  facts): operations of the language, not of the hardware. They stay `hir::Intrinsic`s that
  lowering expands; this design does not change them.

The families are internal: programs use `Math`, `velt/simd`, `Atomic` and the integer methods.

## Memory at an address

std's runtime code in Velt (the async executor of #2649, tracking #2196) needs what the runtime
did in Rust: read and write raw memory, change a word atomically where C or the OS can see it,
and call a heap future's poll and drop functions. These are std-only intrinsics
(`std/lang/intrinsics/address.velt`, `atomic.velt`, `future.velt`); programs and packages reach
none of them, and the ones that touch raw memory are `declare unsafe intrinsic function`, so
their callers are unsafe code with a `// SAFETY:` comment, as for `addressLoad` and `Ptr.at`.

- `addressStore<T>(address: usize, value: T)` is `addressLoad`'s write: one VIR assignment to
  `*(T*)address`, so every backend (LLVM, Cranelift, and wasm through LLVM) emits a plain store.
  Sema holds `T` of both to a number type or `bool` (a type parameter passes; lowering checks
  the instance): nothing that owns memory or holds a reference crosses as raw bytes.
- `ptrAtomicLoad`, `ptrAtomicStore`, `ptrAtomicRmw`, `ptrAtomicCas`, `ptrAtomicCasWeak` take a
  `Ptr<T>` for an integer `T` and do what `Atomic<T>`'s methods do, on the integer it points to:
  the same `velt_atomic_*` operations (velt_vir `atomic.rs`), so the same single instructions
  with the ordering the call names. The orderings, and `ptrAtomicRmw`'s operation (`"add"`,
  `"exchange"`, …), are string literal parameters as on `Atomic`; the declaration's union types
  reject an ordering the operation cannot have, and sema turns the literals into the integer
  literals the atomic intrinsics take (a value that is not a literal is an error). The `Ptr` is
  the caller's promise that an aligned `T` is there and that every thread changes it atomically.
- `atomicAddress(a: Atomic<T>): usize` gives the address of an `Atomic`'s word for the OS's
  wait and wake calls (`futex`, `__ulock_wait`, `WaitOnAddress`). An address is only useful
  while the word stays where it is, so sema accepts two places and nothing else, by the shape of
  the argument: a *static atomic* (a module `const` made by `new Atomic<T>(literal)`, whose
  word is in the program's writable data for the whole run, velt_vir `static_atomic.rs`), and a
  field (of a field…) of the value of a `shared` box read in place (`s.state`,
  `s.inner.waiters`): a box never moves, and its value is immutable apart from its atomics, so
  nothing replaces the field. A local, an object field reached otherwise, an array element (an
  array may grow and move its elements) and a module `let` or computed `const` (module state is
  per task) are errors. The box's lifetime stays the caller's: the address is valid while it
  holds a reference to the `shared` value. `T` is a 32- or 64-bit integer type, the widths the
  OS calls take. Getting the address is safe; using it is unsafe (`Ptr.at`).
- `futPoll(f: usize, cx: usize): u32` and `futDrop(f: usize)` call through a heap future's
  header (rt_abi_async.md §1): the entry at offset 0 or 8 is loaded and called indirectly
  (`vir::Callee::Ptr`) with the C calling convention, as the runtime called it. Lowering's own
  polls and drops of heap futures (awaiting a promise value, `Promise.all`'s children, the drop
  glue of a `Promise<T>`) use the same code (velt_vir `lower/raw_memory.rs`), so
  `velt_rt_fut_poll` and `velt_rt_fut_drop` are gone from the runtime. A header entry is an
  8-byte slot on every target (wasm32 keeps the table index in its low half), so the
  `{ poll, drop }` aggregate is 16 bytes everywhere. The calls do not count as calls of an
  unknown function value in a `read` callback (`Boxing::read_indirect`), as the runtime call did
  not.

What is left for the executor is starting a thread with a C callback, which waits for a decision
on how Velt hands a function to C.

## Adding an intrinsic

1. **Declare it** in `std/lang/intrinsics/<family>.velt` with a doc comment, or, for one that
   std reaches through a runtime function, `declare function velt_rt_<family>_<name>` in the
   std module that uses it.
2. **Register it** in its family's enum in `velt_vir` with its symbol and signature; give it
   its effects in `effects` if it is a runtime function.
3. **Define its reference**: the runtime function, or for an always-inline family the
   semantics in the enum's doc comment.
4. **Fold it** in `eval` where it is pure and the result does not depend on the hardware.
5. **Emit it** in both backends: velt_codegen_cl (`function/terminator.rs` `math_value`, or the
   family's module) and velt_codegen_llvm (`math_target`, or the family's module). A
   CPU-specific one also gets its extension (`Ext`) and its check.
6. **Test it**: the Cranelift test programs (`tests/programs/arith.rs`, JIT, object and linked
   runs), the LLVM native tests (compiled with clang, compared bit for bit), and for a
   folded one `math_fold_tests` against the runtime. Break the code on purpose and see the
   tests fail.
7. **Document it**: rt_abi.md for its symbol and the public API's docs page.

## Steps

1. **Registry** (#1988): `velt_vir::math` and `effects`; the backends and `velt_opt` read it.
2. **Folding** (#1993): `MathOp::eval` and `constfold`.
3. **Statements**: `Rvalue::Intrinsic` added (3a, #1997); `numrep` reads both forms (3b);
   lowering emits the statement form for math operations, then the call form is removed
   from the passes.
4. **A reference for every always-inline operation** and a differential test of each through
   LLVM, Cranelift and wasm; Cranelift calls the reference where it has no instruction instead
   of giving zeros behind a false check.
5. **One CPU-feature model.** Today there are two: `--cpu` gives a compile-time mask of x86
   features (`velt_codegen_cl::cpu::X86_FEATURES`, checked at start-up by
   `velt_rt_check_cpu`; aarch64 has none), and the `Ext` extensions (SHA, carry-less multiply)
   have runtime flags of their own (`velt_rt/src/cpu.rs`), read at every check because
   `has_ext` depends on the architecture only. The step joins them: SHA, PCLMULQDQ, AES,
   AVX-VNNI and AVX512-VNNI appended to the x86 list (additive in rt_abi.md "CPU check"), an
   aarch64 list (sha2, aes/pmull, crc, sha3, dotprod, fp16, i8mm), each `Ext` mapped to its
   feature bit, and a check whose extension `--cpu`'s mask holds the constant `1` in both
   backends. New extensions (`Ext`) for step 7 are rows of these lists.
6. **Scalar operations**: rotate, byte swap, FMA, overflow-reporting and saturating
   arithmetic, `min`/`max` as `llvm.minimum`/`maximum` (JS semantics), `assume`.
7. **Numeric operations for #1866**: f16/bf16 conversions, `mulAdd`, int8 dot products
   (`pmaddubsw`, VNNI `vpdpbusd`, `sdot`/`udot`), widening multiply-add, horizontal sums,
   saturating narrowing, then 256/512-bit vectors split into 128-bit halves where the CPU has
   none (after step 5). The f16/bf16 conversions are designed in #2049: math operations with
   no runtime function (`velt_math_*`, rt_abi.md "Math operations"), one instruction where the
   CPU has it and the branchless integer sequences of `velt_vir::math::half` elsewhere.
8. **HIR**: split `hir::Intrinsic` into language operations and library intrinsics; declare
   the builtins that still have hand-written signatures (`panic`, `sleep`, `xs.push`).

## Not in this design

- Exposing CPU-specific operations to packages (`core::arch`'s model). The gate and the check
  are std-only today; a public `velt/simd/x86` waits for a package that needs it.
- A general `comptime`: folding stays the optimizer's (#1866 area 9).
