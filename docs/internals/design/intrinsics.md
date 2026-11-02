# Design: intrinsics, operations the compiler knows

Status: **proposed** (issue #1981). Steps 1–3a are in pull requests (#1988, #1993, #1997); this
document describes the model they build and the steps after them.

## Problem

An intrinsic is an operation whose meaning the compiler knows instead of a function body it
compiles: `Math.sqrt`, `x.countOnes()`, a SIMD lane add, an atomic compare-and-swap, a
SHA-256 round. Knowing it is what lets the compiler emit one instruction, fold it on constants,
move it out of a loop, or pick the instruction a CPU has.

Velt's front end treats them that way. `std/lang/intrinsics/*.vlt` declares each one
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
  without it never reaches it.
- *Language runtime* (generators, channels, `any`, reflection, weak references, ownership
  facts): operations of the language, not of the hardware. They stay `hir::Intrinsic`s that
  lowering expands; this design does not change them.

The families are internal: programs use `Math`, `velt/simd`, `Atomic` and the integer methods.

## Adding an intrinsic

1. **Declare it** in `std/lang/intrinsics/<family>.vlt` with a doc comment, or, for one that
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
