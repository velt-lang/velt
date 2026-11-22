//! The instruction table (`velt_vir::arch`): a row per velt/arch method, grouped by class. A new
//! instruction is a row here, then `cargo xtask gen-arch` (which rewrites the std methods and
//! the C-name index) and the LLVM test of every row (`velt_codegen_llvm` `tests/arch.rs`).

use super::Llvm::{Call, Ir};
use super::Row;
use super::Val::{self, I32x4, I8x16, U32x4, U64x2, U8x16, U16, U32, U64, U8};
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
];
