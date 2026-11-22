# velt/arch by C intrinsic

Generated from velt_vir's table (`crates/velt_vir/src/arch`) by `cargo xtask gen-arch`: the
velt/arch method for each C intrinsic of Intel's guide and Arm's ACLE that it has (#3222,
`docs/internals/design/package-machine-code.md` §3). A method runs only where the CPU has its
feature: behind its class's `isSupported()`, or in a function gated on the feature.

| C intrinsic | Instruction | velt/arch | Feature |
|---|---|---|---|
| `__crc32b` | `crc32b` | `Crc32.computeCrc32U8` (velt/arch/arm) | `crc-arm` |
| `__crc32cb` | `crc32cb` | `Crc32.computeCrc32CU8` (velt/arch/arm) | `crc-arm` |
| `__crc32cd` | `crc32cx` | `Crc32.computeCrc32CU64` (velt/arch/arm) | `crc-arm` |
| `__crc32ch` | `crc32ch` | `Crc32.computeCrc32CU16` (velt/arch/arm) | `crc-arm` |
| `__crc32cw` | `crc32cw` | `Crc32.computeCrc32CU32` (velt/arch/arm) | `crc-arm` |
| `__crc32d` | `crc32x` | `Crc32.computeCrc32U64` (velt/arch/arm) | `crc-arm` |
| `__crc32h` | `crc32h` | `Crc32.computeCrc32U16` (velt/arch/arm) | `crc-arm` |
| `__crc32w` | `crc32w` | `Crc32.computeCrc32U32` (velt/arch/arm) | `crc-arm` |
| `_andn_u32` | `andn` | `Bmi1.andNotU32` (velt/arch/x86) | `bmi1-x86` |
| `_andn_u64` | `andn` | `Bmi1.andNotU64` (velt/arch/x86) | `bmi1-x86` |
| `_bextr2_u32` | `bextr` | `Bmi1.bitFieldExtractU32` (velt/arch/x86) | `bmi1-x86` |
| `_bextr2_u64` | `bextr` | `Bmi1.bitFieldExtractU64` (velt/arch/x86) | `bmi1-x86` |
| `_blsi_u32` | `blsi` | `Bmi1.extractLowestSetBitU32` (velt/arch/x86) | `bmi1-x86` |
| `_blsi_u64` | `blsi` | `Bmi1.extractLowestSetBitU64` (velt/arch/x86) | `bmi1-x86` |
| `_blsmsk_u32` | `blsmsk` | `Bmi1.getMaskUpToLowestSetBitU32` (velt/arch/x86) | `bmi1-x86` |
| `_blsmsk_u64` | `blsmsk` | `Bmi1.getMaskUpToLowestSetBitU64` (velt/arch/x86) | `bmi1-x86` |
| `_blsr_u32` | `blsr` | `Bmi1.resetLowestSetBitU32` (velt/arch/x86) | `bmi1-x86` |
| `_blsr_u64` | `blsr` | `Bmi1.resetLowestSetBitU64` (velt/arch/x86) | `bmi1-x86` |
| `_bzhi_u32` | `bzhi` | `Bmi2.zeroHighBitsU32` (velt/arch/x86) | `bmi2-x86` |
| `_bzhi_u64` | `bzhi` | `Bmi2.zeroHighBitsU64` (velt/arch/x86) | `bmi2-x86` |
| `_lzcnt_u32` | `lzcnt` | `Lzcnt.leadingZeroCountU32` (velt/arch/x86) | `lzcnt-x86` |
| `_lzcnt_u64` | `lzcnt` | `Lzcnt.leadingZeroCountU64` (velt/arch/x86) | `lzcnt-x86` |
| `_mm_crc32_u16` | `crc32w` | `Sse42.crc32U16` (velt/arch/x86) | `sse4.2-x86` |
| `_mm_crc32_u32` | `crc32l` | `Sse42.crc32U32` (velt/arch/x86) | `sse4.2-x86` |
| `_mm_crc32_u64` | `crc32q` | `Sse42.crc32U64` (velt/arch/x86) | `sse4.2-x86` |
| `_mm_crc32_u8` | `crc32b` | `Sse42.crc32U8` (velt/arch/x86) | `sse4.2-x86` |
| `_mm_popcnt_u32` | `popcnt` | `Popcnt.popCountU32` (velt/arch/x86) | `popcnt-x86` |
| `_mm_popcnt_u64` | `popcnt` | `Popcnt.popCountU64` (velt/arch/x86) | `popcnt-x86` |
| `_mm_shuffle_epi8` | `pshufb` | `Ssse3.shuffle` (velt/arch/x86) | `ssse3-x86` |
| `_pdep_u32` | `pdep` | `Bmi2.parallelBitDepositU32` (velt/arch/x86) | `bmi2-x86` |
| `_pdep_u64` | `pdep` | `Bmi2.parallelBitDepositU64` (velt/arch/x86) | `bmi2-x86` |
| `_pext_u32` | `pext` | `Bmi2.parallelBitExtractU32` (velt/arch/x86) | `bmi2-x86` |
| `_pext_u64` | `pext` | `Bmi2.parallelBitExtractU64` (velt/arch/x86) | `bmi2-x86` |
| `_tzcnt_u32` | `tzcnt` | `Bmi1.trailingZeroCountU32` (velt/arch/x86) | `bmi1-x86` |
| `_tzcnt_u64` | `tzcnt` | `Bmi1.trailingZeroCountU64` (velt/arch/x86) | `bmi1-x86` |
| `vdotq_s32` | `sdot` | `Dp.dotProductI8` (velt/arch/arm) | `dotprod-arm` |
| `vdotq_u32` | `udot` | `Dp.dotProductU8` (velt/arch/arm) | `dotprod-arm` |
| `vmmlaq_s32` | `smmla` | `I8mm.matrixMultiplyAccumulateI8` (velt/arch/arm) | `i8mm-arm` |
| `vmmlaq_u32` | `ummla` | `I8mm.matrixMultiplyAccumulateU8` (velt/arch/arm) | `i8mm-arm` |
| `vmull_high_p64` | `pmull2` | `Pmull.polynomialMultiplyWideningUpper` (velt/arch/arm) | `clmul-arm` |
| `vmull_p64` | `pmull` | `Pmull.polynomialMultiplyWideningLower` (velt/arch/arm) | `clmul-arm` |
| `vusdotq_s32` | `usdot` | `I8mm.dotProductUnsignedSigned` (velt/arch/arm) | `i8mm-arm` |
