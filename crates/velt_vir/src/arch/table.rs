//! The instruction table (`velt_vir::arch`): a row per velt/arch method, grouped by class. A new
//! instruction is a row here, then `cargo xtask gen-arch` (which rewrites the std methods and
//! the C-name index) and the LLVM test of every row (`velt_codegen_llvm` `tests/arch.rs`).

use super::Lane;
use super::Llvm::{Call, Ir};
use super::Row;
use super::Val::{self, I32x4, I8x16, U32x4, U64x2, U8x16, U16, U32, U64, U8, V256, V512};
use crate::simd::Ext;

const V: &str = "value";
const CRC: (&str, Val) = ("crc", U32);

/// A row; `params` and `ret` as the method's.
#[allow(clippy::too_many_arguments)] // the row's columns
const fn row(
    ext: Ext,
    method: &'static str,
    c: &'static str,
    instr: &'static str,
    params: &'static [(&'static str, Val)],
    ret: Val,
    llvm: super::Llvm,
    doc: &'static str,
) -> Row {
    Row {
        ext,
        method,
        c,
        instr,
        params,
        ret,
        llvm,
        doc,
    }
}

const CTPOP32: &[&str] = &["declare i32 @llvm.ctpop.i32(i32)"];
const CTPOP64: &[&str] = &["declare i64 @llvm.ctpop.i64(i64)"];
const CTLZ32: &[&str] = &["declare i32 @llvm.ctlz.i32(i32, i1)"];
const CTLZ64: &[&str] = &["declare i64 @llvm.ctlz.i64(i64, i1)"];
const CTTZ32: &[&str] = &["declare i32 @llvm.cttz.i32(i32, i1)"];
const CTTZ64: &[&str] = &["declare i64 @llvm.cttz.i64(i64, i1)"];
const AARCH64_CRC: &[&str] = &[
    "declare i32 @llvm.aarch64.crc32b(i32, i32)",
    "declare i32 @llvm.aarch64.crc32h(i32, i32)",
    "declare i32 @llvm.aarch64.crc32cb(i32, i32)",
    "declare i32 @llvm.aarch64.crc32ch(i32, i32)",
];
const PMULL: &[&str] = &["declare <16 x i8> @llvm.aarch64.neon.pmull64(i64, i64)"];

// The 256- and 512-bit vectors (`Vector256<T>`, `Vector512<T>`).
const YU8: Val = V256(Lane::U8);
const YI8: Val = V256(Lane::I8);
const YI16: Val = V256(Lane::I16);
const YU32: Val = V256(Lane::U32);
const YI32: Val = V256(Lane::I32);
const YU64: Val = V256(Lane::U64);
const YF32: Val = V256(Lane::F32);
const YF64: Val = V256(Lane::F64);
const ZU8: Val = V512(Lane::U8);
const ZU32: Val = V512(Lane::U32);
const ZU64: Val = V512(Lane::U64);
const ZF32: Val = V512(Lane::F32);
const LR32: &[(&str, Val)] = &[("left", YU32), ("right", YU32)];
const LR8: &[(&str, Val)] = &[("left", YU8), ("right", YU8)];
const ZLR8: &[(&str, Val)] = &[("left", ZU8), ("right", ZU8)];
const ZLR32: &[(&str, Val)] = &[("left", ZU32), ("right", ZU32)];

/// Every instruction.
#[rustfmt::skip]
pub static ROWS: &[Row] = &[
    // x86-64: SSE4.2's CRC32 (the Castagnoli polynomial, as `crc32c`).
    row(Ext::Sse42X86, "crc32U8", "_mm_crc32_u8", "crc32b", &[CRC, ("data", U8)], U32,
        Call("llvm.x86.sse42.crc32.32.8"),
        "The CRC-32C of `crc` extended by the byte `data` (the bits reflected, no final inversion)."),
    row(Ext::Sse42X86, "crc32U16", "_mm_crc32_u16", "crc32w", &[CRC, ("data", U16)], U32,
        Call("llvm.x86.sse42.crc32.32.16"),
        "The CRC-32C of `crc` extended by the two bytes of `data`, low byte first."),
    row(Ext::Sse42X86, "crc32U32", "_mm_crc32_u32", "crc32l", &[CRC, ("data", U32)], U32,
        Call("llvm.x86.sse42.crc32.32.32"),
        "The CRC-32C of `crc` extended by the four bytes of `data`, low byte first."),
    row(Ext::Sse42X86, "crc32U64", "_mm_crc32_u64", "crc32q", &[("crc", U64), ("data", U64)], U64,
        Call("llvm.x86.sse42.crc32.64.64"),
        "The CRC-32C of `crc` (its low 32 bits) extended by the eight bytes of `data`, low byte first."),
    // POPCNT and LZCNT.
    row(Ext::PopcntX86, "popCountU32", "_mm_popcnt_u32", "popcnt", &[(V, U32)], U32,
        Ir { decls: CTPOP32, body: "%r = call i32 @llvm.ctpop.i32(i32 %x0)" },
        "The number of bits set in `value`."),
    row(Ext::PopcntX86, "popCountU64", "_mm_popcnt_u64", "popcnt", &[(V, U64)], U64,
        Ir { decls: CTPOP64, body: "%r = call i64 @llvm.ctpop.i64(i64 %x0)" },
        "The number of bits set in `value`."),
    row(Ext::LzcntX86, "leadingZeroCountU32", "_lzcnt_u32", "lzcnt", &[(V, U32)], U32,
        Ir { decls: CTLZ32, body: "%r = call i32 @llvm.ctlz.i32(i32 %x0, i1 false)" },
        "The number of zero bits above the highest bit set in `value` (32 for 0)."),
    row(Ext::LzcntX86, "leadingZeroCountU64", "_lzcnt_u64", "lzcnt", &[(V, U64)], U64,
        Ir { decls: CTLZ64, body: "%r = call i64 @llvm.ctlz.i64(i64 %x0, i1 false)" },
        "The number of zero bits above the highest bit set in `value` (64 for 0)."),
    // BMI1.
    row(Ext::Bmi1X86, "andNotU32", "_andn_u32", "andn", &[("left", U32), ("right", U32)], U32,
        Ir { decls: &[], body: "%n = xor i32 %x0, -1\n%r = and i32 %n, %x1" },
        "`~left & right`."),
    row(Ext::Bmi1X86, "andNotU64", "_andn_u64", "andn", &[("left", U64), ("right", U64)], U64,
        Ir { decls: &[], body: "%n = xor i64 %x0, -1\n%r = and i64 %n, %x1" },
        "`~left & right`."),
    row(Ext::Bmi1X86, "trailingZeroCountU32", "_tzcnt_u32", "tzcnt", &[(V, U32)], U32,
        Ir { decls: CTTZ32, body: "%r = call i32 @llvm.cttz.i32(i32 %x0, i1 false)" },
        "The number of zero bits below the lowest bit set in `value` (32 for 0)."),
    row(Ext::Bmi1X86, "trailingZeroCountU64", "_tzcnt_u64", "tzcnt", &[(V, U64)], U64,
        Ir { decls: CTTZ64, body: "%r = call i64 @llvm.cttz.i64(i64 %x0, i1 false)" },
        "The number of zero bits below the lowest bit set in `value` (64 for 0)."),
    row(Ext::Bmi1X86, "extractLowestSetBitU32", "_blsi_u32", "blsi", &[(V, U32)], U32,
        Ir { decls: &[], body: "%n = sub i32 0, %x0\n%r = and i32 %x0, %n" },
        "The lowest bit set in `value` alone (0 for 0)."),
    row(Ext::Bmi1X86, "extractLowestSetBitU64", "_blsi_u64", "blsi", &[(V, U64)], U64,
        Ir { decls: &[], body: "%n = sub i64 0, %x0\n%r = and i64 %x0, %n" },
        "The lowest bit set in `value` alone (0 for 0)."),
    row(Ext::Bmi1X86, "resetLowestSetBitU32", "_blsr_u32", "blsr", &[(V, U32)], U32,
        Ir { decls: &[], body: "%m = sub i32 %x0, 1\n%r = and i32 %x0, %m" },
        "`value` without its lowest bit set."),
    row(Ext::Bmi1X86, "resetLowestSetBitU64", "_blsr_u64", "blsr", &[(V, U64)], U64,
        Ir { decls: &[], body: "%m = sub i64 %x0, 1\n%r = and i64 %x0, %m" },
        "`value` without its lowest bit set."),
    row(Ext::Bmi1X86, "getMaskUpToLowestSetBitU32", "_blsmsk_u32", "blsmsk", &[(V, U32)], U32,
        Ir { decls: &[], body: "%m = sub i32 %x0, 1\n%r = xor i32 %x0, %m" },
        "The bits up to and including the lowest bit set in `value` (all of them for 0)."),
    row(Ext::Bmi1X86, "getMaskUpToLowestSetBitU64", "_blsmsk_u64", "blsmsk", &[(V, U64)], U64,
        Ir { decls: &[], body: "%m = sub i64 %x0, 1\n%r = xor i64 %x0, %m" },
        "The bits up to and including the lowest bit set in `value` (all of them for 0)."),
    row(Ext::Bmi1X86, "bitFieldExtractU32", "_bextr2_u32", "bextr", &[(V, U32), ("control", U32)], U32,
        Call("llvm.x86.bmi.bextr.32"),
        "The bits of `value` from bit `control & 0xff`, `(control >> 8) & 0xff` of them, at the bottom."),
    row(Ext::Bmi1X86, "bitFieldExtractU64", "_bextr2_u64", "bextr", &[(V, U64), ("control", U64)], U64,
        Call("llvm.x86.bmi.bextr.64"),
        "The bits of `value` from bit `control & 0xff`, `(control >> 8) & 0xff` of them, at the bottom."),
    // BMI2.
    row(Ext::Bmi2X86, "zeroHighBitsU32", "_bzhi_u32", "bzhi", &[(V, U32), ("index", U32)], U32,
        Call("llvm.x86.bmi.bzhi.32"),
        "`value` with the bits from bit `index & 0xff` up cleared."),
    row(Ext::Bmi2X86, "zeroHighBitsU64", "_bzhi_u64", "bzhi", &[(V, U64), ("index", U64)], U64,
        Call("llvm.x86.bmi.bzhi.64"),
        "`value` with the bits from bit `index & 0xff` up cleared."),
    row(Ext::Bmi2X86, "parallelBitDepositU32", "_pdep_u32", "pdep", &[(V, U32), ("mask", U32)], U32,
        Call("llvm.x86.bmi.pdep.32"),
        "The low bits of `value`, in order, placed at the bits set in `mask` (the others 0)."),
    row(Ext::Bmi2X86, "parallelBitDepositU64", "_pdep_u64", "pdep", &[(V, U64), ("mask", U64)], U64,
        Call("llvm.x86.bmi.pdep.64"),
        "The low bits of `value`, in order, placed at the bits set in `mask` (the others 0)."),
    row(Ext::Bmi2X86, "parallelBitExtractU32", "_pext_u32", "pext", &[(V, U32), ("mask", U32)], U32,
        Call("llvm.x86.bmi.pext.32"),
        "The bits of `value` at the bits set in `mask`, in order, packed at the bottom."),
    row(Ext::Bmi2X86, "parallelBitExtractU64", "_pext_u64", "pext", &[(V, U64), ("mask", U64)], U64,
        Call("llvm.x86.bmi.pext.64"),
        "The bits of `value` at the bits set in `mask`, in order, packed at the bottom."),
    // SSSE3.
    row(Ext::Ssse3X86, "shuffle", "_mm_shuffle_epi8", "pshufb", &[(V, U8x16), ("mask", U8x16)], U8x16,
        Call("llvm.x86.ssse3.pshuf.b.128"),
        "Byte `i` is byte `mask[i] & 15` of `value`, or 0 where bit 7 of `mask[i]` is set."),
    // arm64: CRC32 (the ISO-HDLC polynomial of zlib) and CRC32C.
    row(Ext::CrcArm, "computeCrc32U8", "__crc32b", "crc32b", &[CRC, ("data", U8)], U32,
        Ir { decls: AARCH64_CRC, body: "%d = zext i8 %x1 to i32\n%r = call i32 @llvm.aarch64.crc32b(i32 %x0, i32 %d)" },
        "The CRC-32 of `crc` extended by the byte `data` (the bits reflected, no final inversion)."),
    row(Ext::CrcArm, "computeCrc32U16", "__crc32h", "crc32h", &[CRC, ("data", U16)], U32,
        Ir { decls: AARCH64_CRC, body: "%d = zext i16 %x1 to i32\n%r = call i32 @llvm.aarch64.crc32h(i32 %x0, i32 %d)" },
        "The CRC-32 of `crc` extended by the two bytes of `data`, low byte first."),
    row(Ext::CrcArm, "computeCrc32U32", "__crc32w", "crc32w", &[CRC, ("data", U32)], U32,
        Call("llvm.aarch64.crc32w"),
        "The CRC-32 of `crc` extended by the four bytes of `data`, low byte first."),
    row(Ext::CrcArm, "computeCrc32U64", "__crc32d", "crc32x", &[CRC, ("data", U64)], U32,
        Call("llvm.aarch64.crc32x"),
        "The CRC-32 of `crc` extended by the eight bytes of `data`, low byte first."),
    row(Ext::CrcArm, "computeCrc32CU8", "__crc32cb", "crc32cb", &[CRC, ("data", U8)], U32,
        Ir { decls: AARCH64_CRC, body: "%d = zext i8 %x1 to i32\n%r = call i32 @llvm.aarch64.crc32cb(i32 %x0, i32 %d)" },
        "The CRC-32C of `crc` extended by the byte `data` (the bits reflected, no final inversion)."),
    row(Ext::CrcArm, "computeCrc32CU16", "__crc32ch", "crc32ch", &[CRC, ("data", U16)], U32,
        Ir { decls: AARCH64_CRC, body: "%d = zext i16 %x1 to i32\n%r = call i32 @llvm.aarch64.crc32ch(i32 %x0, i32 %d)" },
        "The CRC-32C of `crc` extended by the two bytes of `data`, low byte first."),
    row(Ext::CrcArm, "computeCrc32CU32", "__crc32cw", "crc32cw", &[CRC, ("data", U32)], U32,
        Call("llvm.aarch64.crc32cw"),
        "The CRC-32C of `crc` extended by the four bytes of `data`, low byte first."),
    row(Ext::CrcArm, "computeCrc32CU64", "__crc32cd", "crc32cx", &[CRC, ("data", U64)], U32,
        Call("llvm.aarch64.crc32cx"),
        "The CRC-32C of `crc` extended by the eight bytes of `data`, low byte first."),
    // 64-bit PMULL.
    row(Ext::ClmulArm, "polynomialMultiplyWideningLower", "vmull_p64", "pmull", &[("left", U64x2), ("right", U64x2)], U64x2,
        Ir { decls: PMULL, body: "%a = extractelement <2 x i64> %x0, i32 0\n%b = extractelement <2 x i64> %x1, i32 0\n%p = call <16 x i8> @llvm.aarch64.neon.pmull64(i64 %a, i64 %b)\n%r = bitcast <16 x i8> %p to <2 x i64>" },
        "The 128-bit carry-less product of lane 0 of `left` and lane 0 of `right`."),
    row(Ext::ClmulArm, "polynomialMultiplyWideningUpper", "vmull_high_p64", "pmull2", &[("left", U64x2), ("right", U64x2)], U64x2,
        Ir { decls: PMULL, body: "%a = extractelement <2 x i64> %x0, i32 1\n%b = extractelement <2 x i64> %x1, i32 1\n%p = call <16 x i8> @llvm.aarch64.neon.pmull64(i64 %a, i64 %b)\n%r = bitcast <16 x i8> %p to <2 x i64>" },
        "The 128-bit carry-less product of lane 1 of `left` and lane 1 of `right`."),
    // The dot products.
    row(Ext::DotprodArm, "dotProductU8", "vdotq_u32", "udot", &[("addend", U32x4), ("left", U8x16), ("right", U8x16)], U32x4,
        Call("llvm.aarch64.neon.udot.v4i32.v16i8"),
        "Lane `i` of `addend` plus the products of bytes `4i` to `4i + 3` of `left` and `right`."),
    row(Ext::DotprodArm, "dotProductI8", "vdotq_s32", "sdot", &[("addend", I32x4), ("left", I8x16), ("right", I8x16)], I32x4,
        Call("llvm.aarch64.neon.sdot.v4i32.v16i8"),
        "Lane `i` of `addend` plus the products of bytes `4i` to `4i + 3` of `left` and `right`."),
    row(Ext::I8mmArm, "matrixMultiplyAccumulateU8", "vmmlaq_u32", "ummla", &[("addend", U32x4), ("left", U8x16), ("right", U8x16)], U32x4,
        Call("llvm.aarch64.neon.ummla.v4i32.v16i8"),
        "`addend` (a 2×2 matrix) plus `left` (2×8, by rows) times `right` (8×2, by columns)."),
    row(Ext::I8mmArm, "matrixMultiplyAccumulateI8", "vmmlaq_s32", "smmla", &[("addend", I32x4), ("left", I8x16), ("right", I8x16)], I32x4,
        Call("llvm.aarch64.neon.smmla.v4i32.v16i8"),
        "`addend` (a 2×2 matrix) plus `left` (2×8, by rows) times `right` (8×2, by columns)."),
    row(Ext::I8mmArm, "dotProductUnsignedSigned", "vusdotq_s32", "usdot", &[("addend", I32x4), ("left", U8x16), ("right", I8x16)], I32x4,
        Call("llvm.aarch64.neon.usdot.v4i32.v16i8"),
        "Lane `i` of `addend` plus the products of the unsigned bytes `4i` to `4i + 3` of `left` and the signed ones of `right`."),
    // AVX: 256-bit floats.
    row(Ext::AvxX86, "addF32", "_mm256_add_ps", "vaddps", &[("left", YF32), ("right", YF32)], YF32,
        Ir { decls: &[], body: "%r = fadd <8 x float> %x0, %x1" },
        "The lanes of `left` plus those of `right`."),
    row(Ext::AvxX86, "multiplyF32", "_mm256_mul_ps", "vmulps", &[("left", YF32), ("right", YF32)], YF32,
        Ir { decls: &[], body: "%r = fmul <8 x float> %x0, %x1" },
        "The lanes of `left` times those of `right`."),
    row(Ext::AvxX86, "addF64", "_mm256_add_pd", "vaddpd", &[("left", YF64), ("right", YF64)], YF64,
        Ir { decls: &[], body: "%r = fadd <4 x double> %x0, %x1" },
        "The lanes of `left` plus those of `right`."),
    row(Ext::AvxX86, "multiplyF64", "_mm256_mul_pd", "vmulpd", &[("left", YF64), ("right", YF64)], YF64,
        Ir { decls: &[], body: "%r = fmul <4 x double> %x0, %x1" },
        "The lanes of `left` times those of `right`."),
    // FMA at 256 bits.
    row(Ext::FmaX86, "multiplyAddF32", "_mm256_fmadd_ps", "vfmadd*ps", &[("a", YF32), ("b", YF32), ("c", YF32)], YF32,
        Call("llvm.fma.v8f32"),
        "`a * b + c` in each lane, rounded once."),
    row(Ext::FmaX86, "multiplyAddF64", "_mm256_fmadd_pd", "vfmadd*pd", &[("a", YF64), ("b", YF64), ("c", YF64)], YF64,
        Call("llvm.fma.v4f64"),
        "`a * b + c` in each lane, rounded once."),
    // AVX2: 256-bit integers.
    row(Ext::Avx2X86, "compareEqualU8", "_mm256_cmpeq_epi8", "vpcmpeqb", LR8, YU8,
        Ir { decls: &[], body: "%c = icmp eq <32 x i8> %x0, %x1\n%r = sext <32 x i1> %c to <32 x i8>" },
        "Each byte all ones where `left`'s equals `right`'s, else 0."),
    row(Ext::Avx2X86, "moveMaskU8", "_mm256_movemask_epi8", "vpmovmskb", &[(V, YU8)], U32,
        Call("llvm.x86.avx2.pmovmskb"),
        "Bit `i` is the top bit of byte `i` of `value`."),
    row(Ext::Avx2X86, "shuffleU8", "_mm256_shuffle_epi8", "vpshufb", &[(V, YU8), ("mask", YU8)], YU8,
        Call("llvm.x86.avx2.pshuf.b"),
        "In each 128-bit half, byte `i` is byte `mask[i] & 15` of the same half of `value`, or 0 where bit 7 of `mask[i]` is set."),
    row(Ext::Avx2X86, "addU8", "_mm256_add_epi8", "vpaddb", LR8, YU8,
        Ir { decls: &[], body: "%r = add <32 x i8> %x0, %x1" },
        "The bytes of `left` plus those of `right`, wrapping."),
    row(Ext::Avx2X86, "addU32", "_mm256_add_epi32", "vpaddd", LR32, YU32,
        Ir { decls: &[], body: "%r = add <8 x i32> %x0, %x1" },
        "The lanes of `left` plus those of `right`, wrapping."),
    row(Ext::Avx2X86, "subtractU32", "_mm256_sub_epi32", "vpsubd", LR32, YU32,
        Ir { decls: &[], body: "%r = sub <8 x i32> %x0, %x1" },
        "The lanes of `left` minus those of `right`, wrapping."),
    row(Ext::Avx2X86, "multiplyLowU32", "_mm256_mullo_epi32", "vpmulld", LR32, YU32,
        Ir { decls: &[], body: "%r = mul <8 x i32> %x0, %x1" },
        "The low 32 bits of each product of `left`'s and `right`'s lanes."),
    row(Ext::Avx2X86, "addU64", "_mm256_add_epi64", "vpaddq", &[("left", YU64), ("right", YU64)], YU64,
        Ir { decls: &[], body: "%r = add <4 x i64> %x0, %x1" },
        "The lanes of `left` plus those of `right`, wrapping."),
    row(Ext::Avx2X86, "permuteVarU32", "_mm256_permutevar8x32_epi32", "vpermd|vpermps", &[(V, YU32), ("indices", YU32)], YU32,
        Call("llvm.x86.avx2.permd"),
        "Lane `i` is lane `indices[i] & 7` of `value`."),
    row(Ext::Avx2X86, "minU8", "_mm256_min_epu8", "vpminub", LR8, YU8,
        Ir { decls: &["declare <32 x i8> @llvm.umin.v32i8(<32 x i8>, <32 x i8>)"], body: "%r = call <32 x i8> @llvm.umin.v32i8(<32 x i8> %x0, <32 x i8> %x1)" },
        "The smaller of each pair of bytes, unsigned."),
    row(Ext::Avx2X86, "maxU8", "_mm256_max_epu8", "vpmaxub", LR8, YU8,
        Ir { decls: &["declare <32 x i8> @llvm.umax.v32i8(<32 x i8>, <32 x i8>)"], body: "%r = call <32 x i8> @llvm.umax.v32i8(<32 x i8> %x0, <32 x i8> %x1)" },
        "The larger of each pair of bytes, unsigned."),
    row(Ext::Avx2X86, "multiplyAddAdjacentI16", "_mm256_madd_epi16", "vpmaddwd", &[("left", YI16), ("right", YI16)], YI32,
        Call("llvm.x86.avx2.pmadd.wd"),
        "Lane `i` is `left[2i] * right[2i] + left[2i + 1] * right[2i + 1]`, signed."),
    row(Ext::Avx2X86, "multiplyAddAdjacentU8I8", "_mm256_maddubs_epi16", "vpmaddubsw", &[("left", YU8), ("right", YI8)], YI16,
        Call("llvm.x86.avx2.pmadd.ub.sw"),
        "Lane `i` is `left[2i] * right[2i] + left[2i + 1] * right[2i + 1]` (`left` unsigned, `right` signed), saturated to `i16`."),
    row(Ext::Avx2X86, "sumAbsoluteDifferencesU8", "_mm256_sad_epu8", "vpsadbw", LR8, YU64,
        Call("llvm.x86.avx2.psad.bw"),
        "Lane `i` is the sum of `|left[j] - right[j]|` over bytes `8i` to `8i + 7`."),
    row(Ext::Avx2X86, "blendVariableU8", "_mm256_blendv_epi8", "vpblendvb", &[("left", YU8), ("right", YU8), ("mask", YU8)], YU8,
        Call("llvm.x86.avx2.pblendvb"),
        "Byte `i` is `right`'s where the top bit of `mask[i]` is set, else `left`'s."),
    row(Ext::Avx2X86, "shiftLeftLogicalVariableU32", "_mm256_sllv_epi32", "vpsllvd", &[(V, YU32), ("count", YU32)], YU32,
        Call("llvm.x86.avx2.psllv.d.256"),
        "Each lane of `value` shifted left by the same lane of `count` (0 from 32 on)."),
    row(Ext::Avx2X86, "shiftRightLogicalVariableU32", "_mm256_srlv_epi32", "vpsrlvd", &[(V, YU32), ("count", YU32)], YU32,
        Call("llvm.x86.avx2.psrlv.d.256"),
        "Each lane of `value` shifted right by the same lane of `count`, zeros in (0 from 32 on)."),
    // AVX-512 F: 512-bit vectors and 16-bit masks.
    row(Ext::Avx512fX86, "addU32", "_mm512_add_epi32", "vpaddd", ZLR32, ZU32,
        Ir { decls: &[], body: "%r = add <16 x i32> %x0, %x1" },
        "The lanes of `left` plus those of `right`, wrapping."),
    row(Ext::Avx512fX86, "multiplyLowU32", "_mm512_mullo_epi32", "vpmulld", ZLR32, ZU32,
        Ir { decls: &[], body: "%r = mul <16 x i32> %x0, %x1" },
        "The low 32 bits of each product of `left`'s and `right`'s lanes."),
    row(Ext::Avx512fX86, "addF32", "_mm512_add_ps", "vaddps", &[("left", ZF32), ("right", ZF32)], ZF32,
        Ir { decls: &[], body: "%r = fadd <16 x float> %x0, %x1" },
        "The lanes of `left` plus those of `right`."),
    row(Ext::Avx512fX86, "compareEqualU32", "_mm512_cmpeq_epi32_mask", "vpcmpeqd", ZLR32, U16,
        Ir { decls: &[], body: "%c = icmp eq <16 x i32> %x0, %x1\n%r = bitcast <16 x i1> %c to i16" },
        "Bit `i` set where lane `i` of `left` equals that of `right`."),
    row(Ext::Avx512fX86, "blendU32", "_mm512_mask_blend_epi32", "vpblendmd|vmovdqa32|vmovdqu32", &[("mask", U16), ("left", ZU32), ("right", ZU32)], ZU32,
        Ir { decls: &[], body: "%m = bitcast i16 %x0 to <16 x i1>\n%r = select <16 x i1> %m, <16 x i32> %x2, <16 x i32> %x1" },
        "Lane `i` is `right`'s where bit `i` of `mask` is set, else `left`'s."),
    row(Ext::Avx512fX86, "permuteVarU32", "_mm512_permutexvar_epi32", "vpermd|vpermps", &[(V, ZU32), ("indices", ZU32)], ZU32,
        Call("llvm.x86.avx512.permvar.si.512"),
        "Lane `i` is lane `indices[i] & 15` of `value`."),
    // AVX-512 BW: bytes and 64-bit masks.
    row(Ext::Avx512bwX86, "addU8", "_mm512_add_epi8", "vpaddb", ZLR8, ZU8,
        Ir { decls: &[], body: "%r = add <64 x i8> %x0, %x1" },
        "The bytes of `left` plus those of `right`, wrapping."),
    row(Ext::Avx512bwX86, "compareEqualU8", "_mm512_cmpeq_epi8_mask", "vpcmpeqb", ZLR8, U64,
        Ir { decls: &[], body: "%c = icmp eq <64 x i8> %x0, %x1\n%r = bitcast <64 x i1> %c to i64" },
        "Bit `i` set where byte `i` of `left` equals that of `right`."),
    row(Ext::Avx512bwX86, "minU8", "_mm512_min_epu8", "vpminub", ZLR8, ZU8,
        Ir { decls: &["declare <64 x i8> @llvm.umin.v64i8(<64 x i8>, <64 x i8>)"], body: "%r = call <64 x i8> @llvm.umin.v64i8(<64 x i8> %x0, <64 x i8> %x1)" },
        "The smaller of each pair of bytes, unsigned."),
    row(Ext::Avx512bwX86, "shuffleU8", "_mm512_shuffle_epi8", "vpshufb", &[(V, ZU8), ("mask", ZU8)], ZU8,
        Call("llvm.x86.avx512.pshuf.b.512"),
        "In each 128-bit quarter, byte `i` is byte `mask[i] & 15` of the same quarter of `value`, or 0 where bit 7 of `mask[i]` is set."),
    row(Ext::Avx512bwX86, "moveMaskU8", "_mm512_movepi8_mask", "vpmovb2m|vpcmpgtb", &[(V, ZU8)], U64,
        Ir { decls: &[], body: "%c = icmp slt <64 x i8> %x0, zeroinitializer\n%r = bitcast <64 x i1> %c to i64" },
        "Bit `i` is the top bit of byte `i` of `value`."),
    // AVX-512 VBMI, CD, BITALG and DQ.
    row(Ext::Avx512vbmiX86, "permuteVarU8", "_mm512_permutexvar_epi8", "vpermb", &[(V, ZU8), ("indices", ZU8)], ZU8,
        Call("llvm.x86.avx512.permvar.qi.512"),
        "Byte `i` is byte `indices[i] & 63` of `value`."),
    row(Ext::Avx512cdX86, "leadingZeroCountU32", "_mm512_lzcnt_epi32", "vplzcntd", &[(V, ZU32)], ZU32,
        Ir { decls: &["declare <16 x i32> @llvm.ctlz.v16i32(<16 x i32>, i1)"], body: "%r = call <16 x i32> @llvm.ctlz.v16i32(<16 x i32> %x0, i1 false)" },
        "The number of zero bits above the highest bit set in each lane (32 for 0)."),
    row(Ext::Avx512bitalgX86, "popCountU8", "_mm512_popcnt_epi8", "vpopcntb", &[(V, ZU8)], ZU8,
        Ir { decls: &["declare <64 x i8> @llvm.ctpop.v64i8(<64 x i8>)"], body: "%r = call <64 x i8> @llvm.ctpop.v64i8(<64 x i8> %x0)" },
        "The number of bits set in each byte."),
    row(Ext::Avx512dqX86, "multiplyLowU64", "_mm512_mullo_epi64", "vpmullq", &[("left", ZU64), ("right", ZU64)], ZU64,
        Ir { decls: &[], body: "%r = mul <8 x i64> %x0, %x1" },
        "The low 64 bits of each product of `left`'s and `right`'s lanes."),
];
