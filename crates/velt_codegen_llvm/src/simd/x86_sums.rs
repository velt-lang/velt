//! `sumWide()` of signed lanes on x86-64, where LLVM's own selection is a widening tree: SSE2
//! has `psadbw` for unsigned bytes and `pmaddwd` for signed 16-bit pairs, which LLVM uses for a
//! plain chain of lane adds only in some contexts (#1981, the #2194 review's x86 rows).
//!
//! - `i8x16`: `psadbw(x ^ 0x80, 0)` sums the bytes biased to unsigned (`x + 128`), two 64-bit
//!   halves; their sum minus 16 × 128 is the signed sum. Exact: every step is integer.
//! - `i16x8`: `pmaddwd(x, 1)` adds the signed lanes in pairs into four `i32`s, which the chain
//!   adds as for `i32x4.sum()`. The pair sums fit (|a + b| ≤ 2^16).
//!
//! Each is one fixed sequence wherever it is inlined: 8 instructions for `i8x16` (16 for the
//! widening tree on x86-64-v2, 13 on v3), 6 for `i16x8` (10).

use velt_vir::simd::Lanes;

use super::Body;

/// The x86 form of `lanes.sumWide()`, or `None` where the generic chain is as good (unsigned
/// bytes already select `psadbw`; other lanes have no cheaper form).
pub(super) fn sum_wide(b: &mut Body, lanes: Lanes) -> Option<String> {
    match lanes {
        Lanes::I8 => Some(i8_sum(b)),
        Lanes::I16 => Some(i16_sum(b)),
        _ => None,
    }
}

fn i8_sum(b: &mut Body) -> String {
    b.declarations
        .push("declare <2 x i64> @llvm.x86.sse2.psad.bw(<16 x i8>, <16 x i8>)".to_string());
    let bias = ["i8 -128"; 16].join(", ");
    let biased = b.inst(format!("xor <16 x i8> %a0, <{bias}>"));
    let halves = b.inst(format!(
        "call <2 x i64> @llvm.x86.sse2.psad.bw(<16 x i8> {biased}, <16 x i8> zeroinitializer)"
    ));
    let lo = b.inst(format!("extractelement <2 x i64> {halves}, i64 0"));
    let hi = b.inst(format!("extractelement <2 x i64> {halves}, i64 1"));
    let total = b.inst(format!("add i64 {lo}, {hi}"));
    let total = b.inst(format!("trunc i64 {total} to i32"));
    b.inst(format!("sub i32 {total}, 2048"))
}

fn i16_sum(b: &mut Body) -> String {
    b.declarations
        .push("declare <4 x i32> @llvm.x86.sse2.pmadd.wd(<8 x i16>, <8 x i16>)".to_string());
    let x = b.arg(0, "<8 x i16>");
    let ones = ["i16 1"; 8].join(", ");
    let pairs = b.inst(format!(
        "call <4 x i32> @llvm.x86.sse2.pmadd.wd(<8 x i16> {x}, <8 x i16> <{ones}>)"
    ));
    let mut total = b.inst(format!("extractelement <4 x i32> {pairs}, i64 0"));
    for k in 1..4 {
        let e = b.inst(format!("extractelement <4 x i32> {pairs}, i64 {k}"));
        total = b.inst(format!("add i32 {total}, {e}"));
    }
    total
}
