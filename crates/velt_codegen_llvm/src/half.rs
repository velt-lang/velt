//! The f16 and bf16 conversions (#2049) as inline helpers: one instruction where the target has
//! it (`fcvt` on arm64 for all three f16 conversions; F16C's `vcvtph2ps`/`vcvtps2ph` on x86 for
//! f16 <-> f32), and otherwise the branchless integer sequences of `velt_vir::math::half` (the
//! reference), line by line, instead of the compiler-rt calls (`__extendhfsf2`, `__truncsfhf2`,
//! `__truncdfhf2`) LLVM would emit for `half` there. bf16 always takes the sequences: x86's
//! AVX512-BF16 conversion flushes subnormals, and Arm's `bfcvt` needs FEAT_BF16, which not every
//! arm64 CPU has.
//!
//! f64 narrows through f32 rounded to odd (`@velt.f64_to_f32.odd`) wherever there is no f64 -> f16
//! instruction: the f32 keeps a sticky bit, and with f32's 24 bits at least two more than the 11 of
//! f16 (8 of bf16), rounding that f32 to nearest-even gives the single rounding of the f64, at
//! every magnitude (f32's range covers both formats' subnormals and overflow). So x86 with F16C
//! narrows with `vcvtps2ph`, and the other paths with the 32-bit sequences, which vectorize where
//! the 64-bit ones don't (#2191).

use velt_vir::math::MathOp;

use crate::runtime::InlineHelper;

/// Which f16 conversions the target has instructions for (`Target::half_native`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HalfNative {
    /// None: every conversion is the integer sequence.
    None,
    /// f16 <-> f32 (x86 with F16C); f64 -> f16 goes through f32 rounded to odd (x86 has no
    /// instruction for it).
    F32,
    /// f16 <-> f32 and f64 -> f16 (arm64).
    All,
}

/// The helper for the conversion `op` (its name and definitions), if `op` is one.
pub(crate) fn helper(op: MathOp, native: HalfNative) -> Option<InlineHelper> {
    let f32_native = native != HalfNative::None;
    Some(match op {
        MathOp::F16ToF32 if f32_native => ("@velt.f16_to_f32", &[F16_TO_F32]),
        MathOp::F16ToF32 => ("@velt.f16_to_f32.soft", &[F16_TO_F32_SOFT]),
        MathOp::F32ToF16 if f32_native => ("@velt.f32_to_f16", &[F32_TO_F16]),
        MathOp::F32ToF16 => ("@velt.f32_to_f16.soft", &[F32_TO_F16_SOFT]),
        MathOp::F64ToF16 if native == HalfNative::All => ("@velt.f64_to_f16", &[F64_TO_F16]),
        MathOp::F64ToF16 if f32_native => (
            "@velt.f64_to_f16.odd",
            &[F64_TO_F32_ODD, F32_TO_F16, F64_TO_F16_ODD],
        ),
        MathOp::F64ToF16 => (
            "@velt.f64_to_f16.odd_soft",
            &[F64_TO_F32_ODD, F32_TO_F16_SOFT, F64_TO_F16_ODD_SOFT],
        ),
        MathOp::Bf16ToF32 => ("@velt.bf16_to_f32", &[BF16_TO_F32]),
        MathOp::F32ToBf16 => ("@velt.f32_to_bf16", &[F32_TO_BF16]),
        MathOp::F64ToBf16 => (
            "@velt.f64_to_bf16.odd",
            &[F64_TO_F32_ODD, F32_TO_BF16, F64_TO_BF16_ODD],
        ),
        MathOp::U64ToBf16 => ("@velt.u64_to_bf16", &[CTLZ_DECL, U64_TO_BF16]),
        // The value is its bits.
        MathOp::F16Bits | MathOp::BitsF16 | MathOp::Bf16Bits | MathOp::BitsBf16 => {
            ("@velt.half_bits", &[HALF_BITS])
        }
        MathOp::I64ToBf16 => (
            "@velt.i64_to_bf16",
            &[CTLZ_DECL, U64_TO_BF16, ABS_DECL, I64_TO_BF16],
        ),
        _ => return None,
    })
}

// One instruction each where the target has it. Narrowing quiets a NaN (`fcvt`, `vcvtps2ph`), but
// LLVM folds `fptrunc (fpext x)` to `x`, so a signalling NaN may stay signalling through a round
// trip. That is allowed (#2459): quieting after `fptrunc` here would cost bulk narrowing 25% to
// 107% for data that never takes such a round trip. `f16x8.narrow` quiets explicitly.
const F16_TO_F32: &str = "define internal float @velt.f16_to_f32(i16 %h) alwaysinline nounwind {
  %x = bitcast i16 %h to half
  %r = fpext half %x to float
  ret float %r
}";
const F32_TO_F16: &str = "define internal i16 @velt.f32_to_f16(float %x) alwaysinline nounwind {
  %h = fptrunc float %x to half
  %r = bitcast half %h to i16
  ret i16 %r
}";
const F64_TO_F16: &str = "define internal i16 @velt.f64_to_f16(double %x) alwaysinline nounwind {
  %h = fptrunc double %x to half
  %r = bitcast half %h to i16
  ret i16 %r
}";

// The integer sequences (`velt_vir::math::half`).
pub(crate) const F16_TO_F32_SOFT: &str =
    "define internal float @velt.f16_to_f32.soft(i16 %h16) alwaysinline nounwind {
  %h = zext i16 %h16 to i32
  %a = and i32 %h, 32767
  %o = shl i32 %a, 13
  %e = and i32 %o, 260046848
  %o1 = add i32 %o, 939524096
  %inf = icmp eq i32 %e, 260046848
  %o1i = add i32 %o1, 939524096
  %o2 = select i1 %inf, i32 %o1i, i32 %o1
  %frac = and i32 %a, 1023
  %payload = icmp ne i32 %frac, 0
  %nan = and i1 %inf, %payload
  %quiet = or i32 %o2, 4194304
  %o3 = select i1 %nan, i32 %quiet, i32 %o2
  %biased = add i32 %o1, 8388608
  %fb = bitcast i32 %biased to float
  %diff = fsub float %fb, 0x3F10000000000000
  %sub = bitcast float %diff to i32
  %zero = icmp eq i32 %e, 0
  %r = select i1 %zero, i32 %sub, i32 %o3
  %s = and i32 %h, 32768
  %s16 = shl i32 %s, 16
  %rs = or i32 %r, %s16
  %f = bitcast i32 %rs to float
  ret float %f
}";
pub(crate) const F32_TO_F16_SOFT: &str =
    "define internal i16 @velt.f32_to_f16.soft(float %x) alwaysinline nounwind {
  %b = bitcast float %x to i32
  %hi = lshr i32 %b, 16
  %sign = and i32 %hi, 32768
  %a = and i32 %b, 2147483647
  %n1 = lshr i32 %a, 13
  %n2 = and i32 %n1, 1023
  %nan = or i32 %n2, 32256
  %fa = bitcast i32 %a to float
  %sum = fadd float %fa, 0x3FE0000000000000
  %sb = bitcast float %sum to i32
  %small = sub i32 %sb, 1056964608
  %odd = and i32 %n1, 1
  %t1 = add i32 %a, -939520001
  %t2 = add i32 %t1, %odd
  %normal = lshr i32 %t2, 13
  %is_small = icmp ult i32 %a, 947912704
  %r1 = select i1 %is_small, i32 %small, i32 %normal
  %is_inf = icmp uge i32 %a, 1199566848
  %r2 = select i1 %is_inf, i32 31744, i32 %r1
  %is_nan = icmp ugt i32 %a, 2139095040
  %r3 = select i1 %is_nan, i32 %nan, i32 %r2
  %r4 = or i32 %r3, %sign
  %r = trunc i32 %r4 to i16
  ret i16 %r
}";
/// f64 to f32 rounded to odd: truncated toward zero, then the last bit set when that dropped
/// anything. `fptrunc` rounds to nearest; when that was inexact, one ulp back toward zero if it
/// rounded away (the widened result's bits above the f64's: the signs are equal, so the unsigned
/// order of the bits is the order of the magnitudes; an overflow to infinity becomes the largest
/// finite f32), then the last bit set. No select, so no branch, and it vectorizes. A NaN is
/// never inexact (`one`), so it stays the quiet NaN `fptrunc` gives, with the payload's top bits.
pub(crate) const F64_TO_F32_ODD: &str =
    "define internal float @velt.f64_to_f32.odd(double %x) alwaysinline nounwind {
  %t = fptrunc double %x to float
  %back = fpext float %t to double
  %inexact = fcmp one double %back, %x
  %bb = bitcast double %back to i64
  %bx = bitcast double %x to i64
  %above = icmp ugt i64 %bb, %bx
  %away = and i1 %inexact, %above
  %tb = bitcast float %t to i32
  %dec = zext i1 %away to i32
  %tz = sub i32 %tb, %dec
  %odd = zext i1 %inexact to i32
  %r = or i32 %tz, %odd
  %f = bitcast i32 %r to float
  ret float %f
}";
const F64_TO_F16_ODD: &str =
    "define internal i16 @velt.f64_to_f16.odd(double %x) alwaysinline nounwind {
  %f = call float @velt.f64_to_f32.odd(double %x)
  %r = call i16 @velt.f32_to_f16(float %f)
  ret i16 %r
}";
const F64_TO_F16_ODD_SOFT: &str =
    "define internal i16 @velt.f64_to_f16.odd_soft(double %x) alwaysinline nounwind {
  %f = call float @velt.f64_to_f32.odd(double %x)
  %r = call i16 @velt.f32_to_f16.soft(float %f)
  ret i16 %r
}";
pub(crate) const BF16_TO_F32: &str =
    "define internal float @velt.bf16_to_f32(i16 %b16) alwaysinline nounwind {
  %w = zext i16 %b16 to i32
  %x = shl i32 %w, 16
  %a = and i32 %x, 2147483647
  %nan = icmp ugt i32 %a, 2139095040
  %mask = sext i1 %nan to i32
  %quiet = and i32 %mask, 4194304
  %r = or i32 %x, %quiet
  %f = bitcast i32 %r to float
  ret float %f
}";
pub(crate) const F32_TO_BF16: &str =
    "define internal i16 @velt.f32_to_bf16(float %f) alwaysinline nounwind {
  %x = bitcast float %f to i32
  %hi = lshr i32 %x, 16
  %nanb = or i32 %hi, 64
  %odd = and i32 %hi, 1
  %t1 = add i32 %x, 32767
  %t2 = add i32 %t1, %odd
  %normal = lshr i32 %t2, 16
  %a = and i32 %x, 2147483647
  %nan = icmp ugt i32 %a, 2139095040
  %r1 = select i1 %nan, i32 %nanb, i32 %normal
  %r = trunc i32 %r1 to i16
  ret i16 %r
}";
const F64_TO_BF16_ODD: &str =
    "define internal i16 @velt.f64_to_bf16.odd(double %x) alwaysinline nounwind {
  %f = call float @velt.f64_to_f32.odd(double %x)
  %r = call i16 @velt.f32_to_bf16(float %f)
  ret i16 %r
}";
const CTLZ_DECL: &str = "declare i64 @llvm.ctlz.i64(i64, i1)";
const ABS_DECL: &str = "declare i64 @llvm.abs.i64(i64, i1)";
const U64_TO_BF16: &str = "define internal i16 @velt.u64_to_bf16(i64 %u) alwaysinline nounwind {
  %lz = call i64 @llvm.ctlz.i64(i64 %u, i1 false)
  %sh = and i64 %lz, 63
  %n = shl i64 %u, %sh
  %top = lshr i64 %n, 56
  %rest = shl i64 %n, 8
  %above = icmp ugt i64 %rest, -9223372036854775808
  %tie = icmp eq i64 %rest, -9223372036854775808
  %odd = trunc i64 %top to i1
  %tie_odd = and i1 %tie, %odd
  %up = or i1 %above, %tie_odd
  %up64 = zext i1 %up to i64
  %t = add i64 %top, %up64
  %carry = lshr i64 %t, 8
  %e0 = sub i64 63, %lz
  %e1 = add i64 %e0, %carry
  %e2 = add i64 %e1, 127
  %e3 = shl i64 %e2, 7
  %frac = and i64 %t, 127
  %bits = add i64 %e3, %frac
  %b16 = trunc i64 %bits to i16
  %zero = icmp eq i64 %u, 0
  %r = select i1 %zero, i16 0, i16 %b16
  ret i16 %r
}";
const I64_TO_BF16: &str = "define internal i16 @velt.i64_to_bf16(i64 %i) alwaysinline nounwind {
  %neg = icmp slt i64 %i, 0
  %a = call i64 @llvm.abs.i64(i64 %i, i1 false)
  %b = call i16 @velt.u64_to_bf16(i64 %a)
  %s = or i16 %b, -32768
  %r = select i1 %neg, i16 %s, i16 %b
  ret i16 %r
}";
/// `w.toBits()` / `f16.fromBits(b)` and the bf16 ones: the value is its bits.
const HALF_BITS: &str = "define internal i16 @velt.half_bits(i16 %x) alwaysinline nounwind {
  ret i16 %x
}";
