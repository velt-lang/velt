# Design: packages that compile to hardware

Status: **accepted direction** (#3222, maintainer review 2026-11-18), revision 2: the review's
answers to the open questions and its changes A–E are written in below. Step 1 is built with this
note (#3230), step 2 next (§2 "As built in step 2"); steps 3–6 follow, one pull request each.

Related: #1981 (intrinsics, intrinsics.md), #2203 (compile-time decorators), #1055 (SIMD),
#1866 (numeric workloads), #2402 (wide multiply, add-with-carry), #3094 (`Ptr<T>`), #2943 /
#2189 (packages compiled once), #2196 (no Rust runtime).

## Problem

How close can a package get to the hardware, compared with a Rust crate? Not close. A package
can't write a type or function that compiles to an instruction std doesn't already wrap, and it
can't use the tools std uses to make its own wrappers cost nothing. Each gap is a check in the
compiler that lets only std through, or something the language doesn't have:

| # | Gap | Where | Rust's stable answer |
|---|---|---|---|
| G1 | No vendor instructions beyond what std wraps (`velt/simd`'s SHA, CLMUL, AES, dot products, `Math.fma`): no CRC32, PDEP/PEXT, `pshufb`/`tbl` by name, AVX2, AVX-512, or NEON beyond the lane API | `@intrinsic` (`intrinsic_decls::declare`); `velt/lang` is internal | `core::arch` |
| G2 | No CPU gating: `targetFeature("…")` is read only in std functions (`body/gated.rs` `fn_target`), the checks are seven std methods (`cpu_guard`), `CpuTarget` is a closed enum of seven targets | `is_std` in `gated.rs` | `#[target_feature]`, `is_x86_feature_detected!` |
| G3 | No inlining control: `@inline` / `@noInline` were refused outside std | `decorators/known.rs` (until step 1) | `#[inline]`, `#[inline(always)]`, `#[inline(never)]`, `#[cold]` |
| G4 | No layout promise for value types: a `struct` is inline and Copy (semantics-stage2.md §7), but nothing promises that a one-field struct has its field's ABI, or an alignment | not in the language | `#[repr(transparent)]`, `#[repr(align(n))]` |
| G5 | No register types wider than 128 bits: `f32x8` is two halves in std | `SimdTy` | `__m256`, `__m512`, … |
| G6 | No inline assembly | not in the language | `asm!` |
| G7 | No way to replace a runtime role (allocator, uncaught-error handler) | `std_hooks` roles | `#[global_allocator]`, `#[panic_handler]` |

Not missing: user value types (`struct`), operators on them (operators.md), the scalar bit and
math operations as integer methods (`rotateLeft`, `mulHigh`, `countOnes`, `swapBytes`,
`saturatingAdd`, `checkedAdd`, …), add-with-carry once #2402 lands, and raw memory through
`Ptr<T>` (#3094). This design builds on those.

## Decision: `@intrinsic` stays private; packages get a stable public layer

Letting packages write `@intrinsic declare function` is the obvious fix and the wrong one:

- **The intrinsic table is the compiler's internal interface.** Its names and signatures change
  with nearly every pull request (#2196 alone adds dozens); a package that names one breaks on the
  next release, with no version to depend on.
- **Many intrinsics are language-runtime operations** (`anyDelete`, the weak references,
  generator and future steps, `addressStore`, reference-count facts) that break memory safety when
  misused, and most aren't `unsafe` because only std calls them.
- **Rust doesn't allow it either.** `core::intrinsics` and `link_llvm_intrinsics` are permanently
  unstable; crates reach the hardware through `core::arch`, `#[target_feature]`, `#[inline]`,
  `#[repr]` and `asm!`, a stable surface the compiler maps onto its internals.

So std declares a **public, stable, generated layer** with `@intrinsic` as today, and packages and
programs import it like any std module. The gate on `@intrinsic` stays; once `velt/arch` exists
(step 4) its diagnostic gets a note pointing there.

## 1. Inlining decorators for every module (G3) — step 1

`velt/compiler` (a public module, like .NET's `System.Runtime.CompilerServices`) re-exports
`inline` and `noInline` from std/lang/intrinsics/decorators.velt, where they stay defined; the
compiler finds them by their `std_hooks` roles, never by name, as before.

```ts
import { inline, noInline } from "velt/compiler";

export struct Fixed {
  readonly raw: i64;
  @inline [Symbol.add](o: Fixed): Fixed { return { raw: this.raw + o.raw }; }
}
```

**`@inline` is a hint; `@inline("always")` forces** (review, smaller points). The declaration is
`inline(how: "hint" | "always" = "hint")`: a decorator parameter's default is checked at the
declaration and filled in at every use (`hir::Decorator::args`), so lowering always sees the
argument.

- `@inline("always")` is the old `@inline`: every call inlined in release builds whatever the
  callee's size (`vir::Inline::Always`, LLVM `alwaysinline`), refused on a recursive function
  (`body::inline_markers`).
- `@inline`, the hint, is Rust's `#[inline]`: the body is offered to callers and the inliner
  should prefer it. While a program is compiled as one unit every body is offered already, so
  lowering gives the hint `vir::Inline::Default` and it changes no code yet. It starts to matter
  with units (#2943), where it makes the body travel with the unit's interface; that step adds
  `vir::Inline::Hint` (LLVM `inlinehint`, a raised `velt_opt` threshold), an additive contract
  change made with the units, not before. A hint on a recursive function is allowed.
- `@noInline` is unchanged (`vir::Inline::Never`).
- The other checks are unchanged and now apply to every module: one of the two decorators, on a
  function or method with a body, not abstract, not async, not a generator.
- **std writes `@inline("always")`**, every former use rewritten, and **a bare `@inline` in std
  is an error** ("std's `@inline` says how"). A pull request written before the hint existed
  would otherwise lose its forcing without a word when rebased.

**Cache keys (#2943).** Today only std is compiled separately (the std cache); packages are
compiled with the program, and #3173 imports an `@inline("always")` std function into every LLVM
unit that calls it. Neither changes here: a package's `@inline` function is inlined within the
one program unit, so no cache can go stale. When units land, a unit's cache key must include the
bodies of the `@inline` / `@inline("always")` functions it imports from other units (their body
hashes, recorded in each unit's interface), not only the other units' interfaces; otherwise
changing an inlined operator in package A leaves package B's cached object stale. That rule
belongs to #2943's step 4 ("bodies for callers") and is recorded there.

**`@cold` is not in step 1** (corrected from revision 1). `vir::Function::cold` means "no call of
it returns normally": `velt_opt`'s `cold_calls` merges calls of such functions into one block
followed by `Unreachable`, and `inline::never_returns` reads it. Setting it on a function that
returns would be unsound. `@cold` needs its own VIR flag (a contract change: layout and LLVM
`cold` without the no-return facts), so it moves to a later step with that change.

## 2. CPU features for packages (G2) — steps 2 and 3

**`@targetFeature(...)` replaces the first-statement call**, and each instruction set is a class
whose `isSupported` is the check (review, question 1; .NET's
`System.Runtime.Intrinsics.X86.Sse42.IsSupported`):

```ts
import { targetFeature } from "velt/compiler";
import { Sse42 } from "velt/arch/x86";
import { Crc32 } from "velt/arch/arm";

@targetFeature("sse4.2")
function crcX86(crc: u64, words: u64[]): u64 {
  for (const w of words) crc = Sse42.X64.crc32(crc, w);
  return crc;
}

@targetFeature("crc")
function crcArm(crc: u32, words: u64[]): u32 {
  for (const w of words) crc = Crc32.Arm64.crc32cd(crc, w);
  return crc;
}

export function crc32c(words: u64[]): u32 {
  // Dispatch once, outside the loop (§2 "Checks inside loops").
  if (Sse42.isSupported) return (crcX86(0xffffffff, words) as u32) ^ 0xffffffff;
  if (Crc32.isSupported) return crcArm(0xffffffff, words) ^ 0xffffffff;
  return crcPortable(words);
}
```

- **The rule is today's:** a call of a gated function is inside a function gated on a superset of
  its features, or behind a check that implies them. sema finds `X.isSupported` by definition, as
  `cpu_guard` does today, and takes `&&` of several checks. `isSupported` is true at compile time
  when `--cpu` has the feature; false on the other architecture, on wasm and in Cranelift builds
  (`LowerOptions::no_cpu_extensions`), so gated code is never lowered there; otherwise the
  start-up feature set (A below).
- **Function values of gated functions** (review B). A gated function taken as a value
  (`const f = crcX86`, passed as a callback, stored in a field, a bound method) could be called
  later without a check. Rust refuses the coercion of a `#[target_feature]` function to a safe
  function pointer; Velt does the same: a gated function, method or arrow function can't be used
  as a value except inside a function gated on a superset of its features. Allowing it inside
  `unsafe` is a later option. The error names the function and the features.
- **Checks inside loops** (review C). #3198 measured 22% on a bf16 dot-product kernel from a check
  re-read every iteration. Hoisting a loop-invariant check is the optimizer's job (`velt_opt`'s
  loop-invariant motion treats the feature read as a pure load of a start-up constant), but the
  documented pattern is to dispatch once, outside the loop, as `crc32c` does, and a lint
  (`cpu-check-in-loop`, warning) reports an `isSupported` check inside a loop body when nothing in
  the loop changes its answer, which is always. The lint matters because the cost doesn't show in
  debug builds, where the check is the constant `false`.
- std's `targetFeature("aes-arm")` calls and the seven `hasShaArm()`-style checks become the
  decorator and `isSupported`. **The old checks stay for one release** as `@deprecated` wrappers
  (review, question 4): `u32x4.hasShaX86()` is in the public docs.

**As built in step 2.** `@targetFeature(target)` is a compile-time decorator in
std/lang/intrinsics/decorators.velt, re-exported by `velt/compiler` and found by its `std_hooks`
role (`decorator_target_feature`), on a function of any module. Its parameter is the union of
today's seven targets (`"sha-arm"`, `"sha-x86"`, `"clmul-x86"`, `"clmul-arm"`, `"aes-x86"`,
`"aes-arm"`, `"fma-x86"`), so an unknown name is a type error at the use, and its type
(`FunctionDecorator`) keeps it off methods. A function declared inside another may be gated, with
its calls checked like any other's. The intrinsic `targetFeature` and its first-statement form are
gone; std's 31 gated functions carry the decorator. The checks stay velt/simd's methods
(`u64x2.hasClmulX86()`, …), which packages can already call, and which sema already found by
definition.

Two parts of the plan above were already true or move:

- **B was already enforced** (`body/gated.rs` `gated_value`): a gated function can't be stored,
  passed or returned, and a closure inside one starts without its target. Step 2 adds tests that
  this holds outside std.
- **The `isSupported` classes move to step 3.** They name instruction sets (`Sse42`, `Bmi2`,
  `Crc32`), which only exist once the feature model has them. Naming classes after today's seven
  bundled targets would be renamed in step 3. So would the `cpu-check-in-loop` lint (C), which
  checks those classes: it ships with them in step 3.

**One CPU-feature model** (step 3, = #1981 step 5). `hir::CpuTarget` becomes `CpuFeatures`, a bit
set over two lists: x86 (SSE4.2 … AVX-512, SHA, PCLMULQDQ, AES, BMI1/2, POPCNT, LZCNT, F16C, FMA,
VNNI) and aarch64 (neon, aes, pmull, sha2, sha3, crc, dotprod, fp16, i8mm, bf16).

**CPU detection moves to std** (review A). Today's flags come from `crates/velt_rt/src/cpu.rs`;
step 3 must not add bits to that Rust (#2196). `velt/arch/x86` gets `cpuid` and `xgetbv` (two
intrinsics of the arch table, the first rows of step 4, built in step 3); aarch64 reads
`getauxval(AT_HWCAP/AT_HWCAP2)` on Linux, `sysctlbyname("hw.optional.…")` on macOS and
`IsProcessorFeaturePresent` on Windows through `declare c` (c-ffi.md). A std module of its own
(`std/arch/detect/`) computes the feature set once at start-up into a process static (#2779),
which the checks read and `VELT_CPU_DISABLE` masks (D below). `cpu.rs` is deleted in the same
step. `velt_rt_check_cpu` (the `--cpu` start-up check) reads the same static.

**Contract changes (rule 1, for review in steps 2–3):** `hir::CpuTarget` → `CpuFeatures`;
`std_hooks` gets `decorator_target_feature`; rt_abi.md "CPU check" loses the runtime's flags for
the std static.

## 3. `velt/arch`: vendor instructions (G1) — step 4

Public modules `velt/arch/x86`, `velt/arch/arm`, later `velt/arch/wasm`, shaped as .NET's
`System.Runtime.Intrinsics` (review, question 1): **one class per instruction set, camelCase
methods, a nested class for the 64-bit-only forms**: `Sse42.X64.crc32(crc, data)`,
`Bmi2.X64.parallelBitDeposit(x, mask)`, `Ssse3.shuffle(a, b)`, `AdvSimd.Arm64.vectorTableLookup`.

- **It reads as Velt** (camelCase, no lint exception), and discovery is TypeScript's: `Sse42.`
  lists the instruction set.
- **Each method's doc comment names the C intrinsic** (`_mm_crc32_u64`) and the instruction, so
  the Intel guide and the ACLE stay searchable through the docs; a generated reverse index page
  maps C names to Velt names for porting.
- **Types:** 128-bit operations take and return `velt/simd`'s lane types (`u8x16`, `f32x4`, …).
- **Safety:** a call is safe inside a function gated on the instruction set's features (or
  behind its `isSupported`) and an error anywhere else (Rust's target_feature 1.1). An operation
  that reads or writes memory takes a `Ptr<T>` (#3094) and is `unsafe`.
- **One table, generated:** a checked-in spec (`crates/velt_vir/src/arch/{x86,arm}.toml`, a row
  per instruction: Velt class and name, C name, signature, features, and the LLVM intrinsic or IR
  pattern) feeds `cargo xtask gen-arch`, which writes the std declarations
  (`std/lang/intrinsics/arch_*.velt`, re-exported by `velt/arch/*`), their doc comments,
  `velt_vir::arch::ArchOp` and the reverse index. A test checks the generated files are current.
- **Lowering:** `Rvalue::Intrinsic(Intrinsic::Arch(op), …)` for the pure ones; a call of the op's
  extern for the ones with memory effects, as intrinsics.md's families do. **Only LLVM implements
  them**: a CPU-specific operation is reachable only in gated code, and Cranelift never lowers
  gated code (intrinsics.md), so the table costs one backend. The verifier rejects an `Arch`
  operation in a function not gated on its features.
- **No cost when unused:** lazy std (lazy-std.md).
- **First batch, by demand:** CRC32, BMI1/2, POPCNT and LZCNT by name, `pshufb` / `movemask` /
  `palignr`, `tbl` / `tbx` / `ext`, `vmull_p64`, the dotprod and i8mm dot products, `prefetch`.

**Testing both paths** (review D). Debug builds answer every check false, so `velt test` in debug
only tests the portable path. Step 4 adds:

- `velt test` runs a package's tests twice when the package (or a dependency it calls into) uses
  `velt/arch` or `@targetFeature`: debug, then release, reporting both. `--debug-only` skips the
  second.
- `VELT_CPU_DISABLE=<feature,…>` masks features in the start-up set, so a release build runs the
  fallback on a CPU that has the instructions (as `VELT_SIMD_DISABLE=clmul` does today; it becomes
  an alias). A test matrix runs a package's release tests with and without its features.
- The LLVM native tests compare each table row with clang; they test the instructions, these two
  test the packages.

## 4. 256- and 512-bit register types (G5) — step 5

`velt/arch/x86` adds the opaque register types `Vector256<T>` / `Vector512<T>` (.NET's names,
lane type `T`) and `Mask8` … `Mask64`: new `SimdTy` widths (a hir contract change) that sema
allows as values only in functions gated on `avx` / `avx512f`. Outside gated code they can be
stored (fields, arrays) but not operated on. `.lower()` / `.upper()` and `Vector256.create(lo,
hi)` convert to and from `velt/simd`'s 128-bit types. LLVM lowers them as `<8 x float>` and so on.

**Alignment** (review E). rt_abi.md "SIMD" gives `V128` 8-byte alignment in memory, with
unaligned loads and stores; the wide types are the same: **8-byte aligned in fields and arrays,
and every arch load and store is the unaligned form** (`vmovups`, `vmovdqu`, `vmovdqu8`/`16`/
`32`/`64`), which costs nothing on aligned data on every CPU since Haswell and Zen. The aligned
forms (`vmovaps`, `vmovdqa64`, whose AVX-512 forms fault on a misaligned address) are not in the
table until `@align(32/64)` (step 6) exists, and then only on a `Ptr<T>` to a type declared with
that alignment, so sema can prove the address aligned; never on an arbitrary address.

After this, `velt/simd`'s `f32x8` can lower to one `ymm` register inside gated code; that is a
separate step on `velt/simd`.

## 5. Layout decorators for value types (G4) — step 6

- `@repr("transparent")` on a `struct` with exactly one non-zero-size field gives the struct its
  field's ABI, in registers and through `declare c`. velt_vir already passes the field alone
  (SROA), so this is a promise and a check. It is **checked against clang** the way #3220 checks C
  struct layouts (review): for each C calling convention Velt targets (SysV x86-64, AAPCS64,
  Darwin arm64, Windows x64), a transparent struct of `f64`, `f32`, `u64`, `u8` and `Ptr<T>`
  passes and returns exactly as its field. SysV classifies a struct of one `f64` as SSE like the
  `f64`, but the test is the proof.
- `@align(n)` on a struct: a power of two, at least the natural alignment; arrays are padded.

Both are compile-time decorators (#2203) in `velt/compiler`.

## 6. What a numeric package writes

```ts
import { inline } from "velt/compiler";

/** An unsigned 128-bit integer: two u64 limbs, in two registers, operators inlined. */
export struct U128 {
  readonly lo: u64;
  readonly hi: u64;

  @inline [Symbol.add](o: U128): U128 {
    const [lo, carry] = this.lo.addWithCarry(o.lo, 0); // #2402 (its name is settled there)
    return { lo, hi: this.hi + o.hi + carry };
  }
}

const x = U128.from(5); // no literals for user types (below)
```

Once #2402 lands, `a + b` compiles to `adds`/`adc` (arm64) or `add`/`adc` (x86-64), as Rust's
`u128`, which a test then checks by instruction count against a Rust baseline.

## Not in this design

- **Inline assembly (G6):** Cranelift has none, so every `asm` block would need the check-and-
  fallback shape of §2; `velt/arch` covers what Rust code mostly uses `asm!` for outside OS and
  embedded work. Its own issue when a package needs it.
- **Replacing runtime roles (G7):** after #2196 has moved them into std.
- **Literals for user types** (`const x: U128 = 5`): a follow-up issue (review, question 3).
  TypeScript has no implicit conversion; C#'s `implicit operator` is what this would become, and
  it needs its own design. `U128.from(5)` until then.
- **A package naming an LLVM intrinsic directly:** rejected (one backend, one LLVM version,
  nothing checks its types or effects; Rust keeps it unstable).
- **`Numeric` (#3197) for user types:** rejected: it means a primitive that can be reinterpreted
  as bytes. Generic numeric code over user types uses operator bounds (operators.md).
- **New primitive types from packages** (`i128`, `f8`): compiler decisions; packages build them as
  §6 does.

## Steps

1. **This note; `@inline` (hint) / `@inline("always")` / `@noInline` for every module; std on
   `@inline("always")`; `velt/compiler`.** Tests (crates/veltc/tests/inline_markers.rs): a
   program's `@inline("always")` function from `velt/compiler` has every call inlined in release
   VIR, the hint and `@noInline` keep the calls; the recursion, both-decorators and async errors
   outside std; a bare `@inline` in std is an error.
2. **`@targetFeature` for every module, for today's seven targets; std migrated** (built: §2 "As
   built in step 2"). The rule for function values (B) was already enforced.
3. **One CPU-feature model, detection in std (A), `isSupported`, the in-loop lint (C):**
   `CpuFeatures`, the two lists, `cpuid` / `xgetbv` / the OS calls, the start-up static, `--cpu`
   folding; `velt_rt/src/cpu.rs` deleted. The instruction-set classes and their `isSupported`, the
   `cpu-check-in-loop` lint, and velt/simd's old checks deprecated (moved here from step 2).
4. **`velt/arch/x86` and `velt/arch/arm`, 128-bit:** the spec, `xtask gen-arch`, `Intrinsic::Arch`,
   LLVM lowering, the verifier rule, the first batch, the reverse index, `velt test`'s release run
   and `VELT_CPU_DISABLE` (D).
5. **`Vector256` / `Vector512`, unaligned loads and stores (E), the AVX2 / AVX-512 batches.**
6. **`@repr("transparent")` (checked against clang) and `@align(n)`**, then the aligned forms of
   step 5's loads and stores; `@cold` with its own VIR flag.

## Cost

No run-time cost when the dispatch is outside the loop (§2 "Checks inside loops"): everything
resolves at compile time to what std already emits, and a check hoisted out of a loop is one load
of a start-up constant per call of the dispatching function. A check left inside a hot loop is
not free (#3198: 22%), which is why the lint exists. Build cost: a table lookup per `velt/arch`
call; the generated declarations load only when imported. Step 1 changes no code for any program
(std's markers keep their meaning, and the hint is `Inline::Default`), so `bench/` stays within
noise; step 4 adds no work to Cranelift builds, and `velt test`'s second run applies only to
packages that use `velt/arch`.
