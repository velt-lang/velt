//! The scalar math operations: what each `velt_rt_math_*`, `velt_rt_pow_*` and `velt_math_*`
//! extern means (rt_abi.md "Math"), and what every runtime function may do to memory
//! ([`Effects`]).
//!
//! Those named `velt_rt_*` are real runtime functions (velt_rt's `math.rs`), which are the
//! reference for what each computes; backends emit most of them inline (one instruction, or a
//! short sequence with the runtime call on a cold path). Those named `velt_math_*` have no
//! runtime function: every backend emits them inline, as the SIMD externs, and
//! [`MathOp::eval`] is their reference. Optimizers treat all of them as arithmetic. This module
//! is the one place that says which symbols those are, with their signatures: backends and
//! passes match on [`MathOp`], not on symbol strings, and the verifier rejects one of these
//! externs whose signature is not its operation's.

use crate::vir::{ExternFn, Ty};

/// The f16/bf16 conversions' reference (`velt_common::half`, which sema shares for literals).
pub use velt_common::half;

/// A scalar math operation of the runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MathOp {
    /// `(f64) -> f64`: `Math.sqrt`.
    Sqrt,
    /// `(f64) -> f64`: `Math.floor`.
    Floor,
    /// `(f64) -> f64`: `Math.ceil`.
    Ceil,
    /// `(f64) -> f64`: `Math.round`, ties toward +Infinity, `-0` for `[-0.5, -0]` (JS's, not
    /// C's `round`).
    Round,
    /// `(f64) -> f64`: `Math.trunc`.
    Trunc,
    /// `(f64) -> f64`: `Math.abs`.
    Abs,
    /// `(f64, f64) -> f64`: `a ** b` with JS semantics (always a runtime call).
    PowF64,
    /// `(i64, i64) -> i64`: wrapping integer power (always a runtime call).
    PowI64,
    /// `(f64) -> u64`: the IEEE 754 bits, `x.toBits()`.
    F64Bits,
    /// `(u64) -> f64`: the double with those bits, `f64.fromBits(b)`.
    BitsF64,
    /// `(u64, u64) -> u64`: the high 64 bits of the 128-bit product, `Math.umulh`.
    Umulh,
    /// `(f64) -> i32`: JS's ToInt32.
    ToInt32,
    /// `(i32, i32) -> i32`: JS's `(a * b) | 0` on int32 numbers.
    MulInt32,
    /// `(i32, f64) -> i32`: JS's `(a + x) | 0` for an int32 `a`.
    AddInt32,
    /// `(i32) -> i32`: `Math.clz32` of a ToInt32 value (32 for 0).
    Clz32,
    /// `(u64) -> u64`: leading zero bits (64 for 0), `x.leadingZeros()`.
    Clz64,
    /// `(u64) -> u64`: trailing zero bits (64 for 0), `x.trailingZeros()`.
    Ctz64,
    /// `(u64) -> u64`: set bits, `x.countOnes()`.
    Popcount64,
    /// `(f32) -> u32`: the IEEE 754 bits, `x.toBits()` (no runtime function).
    F32Bits,
    /// `(u32) -> f32`: the float with those bits, `f32.fromBits(b)` (no runtime function).
    BitsF32,
    /// `(u16) -> f32`: the f16 with those bits, widened (exact; [`half`]; no runtime function).
    F16ToF32,
    /// `(f32) -> u16`: the bits of the nearest f16 (no runtime function).
    F32ToF16,
    /// `(f64) -> u16`: the bits of the nearest f16, rounded once (no runtime function).
    F64ToF16,
    /// `(u16) -> f32`: the bf16 with those bits, widened (exact; no runtime function).
    Bf16ToF32,
    /// `(f32) -> u16`: the bits of the nearest bf16 (no runtime function).
    F32ToBf16,
    /// `(f64) -> u16`: the bits of the nearest bf16, rounded once (no runtime function).
    F64ToBf16,
    /// `(i64) -> u16`: the bits of the bf16 nearest the integer, rounded once (no runtime
    /// function).
    I64ToBf16,
    /// `(u64) -> u16`: the bits of the bf16 nearest the integer, rounded once (no runtime
    /// function).
    U64ToBf16,
    /// `(u16) -> u16`: an `f16`'s bits, `w.toBits()` (the identity: the value is its bits).
    F16Bits,
    /// `(u16) -> u16`: the `f16` with those bits, `f16.fromBits(b)` (the identity).
    BitsF16,
    /// `(u16) -> u16`: a `bf16`'s bits (the identity).
    Bf16Bits,
    /// `(u16) -> u16`: the `bf16` with those bits (the identity).
    BitsBf16,
}

impl MathOp {
    /// Every operation.
    pub const ALL: [MathOp; 32] = [
        MathOp::Sqrt,
        MathOp::Floor,
        MathOp::Ceil,
        MathOp::Round,
        MathOp::Trunc,
        MathOp::Abs,
        MathOp::PowF64,
        MathOp::PowI64,
        MathOp::F64Bits,
        MathOp::BitsF64,
        MathOp::Umulh,
        MathOp::ToInt32,
        MathOp::MulInt32,
        MathOp::AddInt32,
        MathOp::Clz32,
        MathOp::Clz64,
        MathOp::Ctz64,
        MathOp::Popcount64,
        MathOp::F32Bits,
        MathOp::BitsF32,
        MathOp::F16ToF32,
        MathOp::F32ToF16,
        MathOp::F64ToF16,
        MathOp::Bf16ToF32,
        MathOp::F32ToBf16,
        MathOp::F64ToBf16,
        MathOp::I64ToBf16,
        MathOp::U64ToBf16,
        MathOp::F16Bits,
        MathOp::BitsF16,
        MathOp::Bf16Bits,
        MathOp::BitsBf16,
    ];

    /// The runtime function's symbol.
    pub const fn symbol(self) -> &'static str {
        match self {
            MathOp::Sqrt => "velt_rt_math_sqrt",
            MathOp::Floor => "velt_rt_math_floor",
            MathOp::Ceil => "velt_rt_math_ceil",
            MathOp::Round => "velt_rt_math_round",
            MathOp::Trunc => "velt_rt_math_trunc",
            MathOp::Abs => "velt_rt_math_fabs",
            MathOp::PowF64 => "velt_rt_pow_f64",
            MathOp::PowI64 => "velt_rt_pow_i64",
            MathOp::F64Bits => "velt_rt_math_f64_bits",
            MathOp::BitsF64 => "velt_rt_math_bits_f64",
            MathOp::Umulh => "velt_rt_math_umulh",
            MathOp::ToInt32 => "velt_rt_math_to_int32",
            MathOp::MulInt32 => "velt_rt_math_mul_int32",
            MathOp::AddInt32 => "velt_rt_math_add_int32",
            MathOp::Clz32 => "velt_rt_math_clz32",
            MathOp::Clz64 => "velt_rt_math_clz64",
            MathOp::Ctz64 => "velt_rt_math_ctz64",
            MathOp::Popcount64 => "velt_rt_math_popcount64",
            MathOp::F32Bits => "velt_math_f32_bits",
            MathOp::BitsF32 => "velt_math_bits_f32",
            MathOp::F16ToF32 => "velt_math_f16_to_f32",
            MathOp::F32ToF16 => "velt_math_f32_to_f16",
            MathOp::F64ToF16 => "velt_math_f64_to_f16",
            MathOp::Bf16ToF32 => "velt_math_bf16_to_f32",
            MathOp::F32ToBf16 => "velt_math_f32_to_bf16",
            MathOp::F64ToBf16 => "velt_math_f64_to_bf16",
            MathOp::I64ToBf16 => "velt_math_i64_to_bf16",
            MathOp::U64ToBf16 => "velt_math_u64_to_bf16",
            MathOp::F16Bits => "velt_math_f16_bits",
            MathOp::BitsF16 => "velt_math_bits_f16",
            MathOp::Bf16Bits => "velt_math_bf16_bits",
            MathOp::BitsBf16 => "velt_math_bits_bf16",
        }
    }

    /// Whether velt_rt defines the operation's symbol (`velt_rt_*`), which a backend may call
    /// where it has no inline code; a `velt_math_*` operation is always emitted inline.
    pub fn has_runtime_function(self) -> bool {
        self.symbol().starts_with("velt_rt_")
    }

    /// The operation's name: the symbol without its `velt_rt_math_` / `velt_rt_` /
    /// `velt_math_` prefix (`sqrt`, `pow_f64`, `f32_bits`).
    pub fn name(self) -> &'static str {
        let s = self.symbol();
        s.strip_prefix("velt_rt_math_")
            .or_else(|| s.strip_prefix("velt_rt_"))
            .or_else(|| s.strip_prefix("velt_math_"))
            .unwrap_or(s)
    }

    /// The operation the runtime function `symbol` is, if it is one.
    pub fn of_symbol(symbol: &str) -> Option<MathOp> {
        MathOp::ALL.into_iter().find(|op| op.symbol() == symbol)
    }

    /// The operation the extern `e` is, if it is one with the operation's signature (which the
    /// verifier guarantees for every extern of a verified program).
    pub fn of_extern(e: &ExternFn) -> Option<MathOp> {
        let op = MathOp::of_symbol(&e.symbol)?;
        let (params, ret) = op.signature();
        (e.params.as_slice() == params && e.ret == ret && !e.noreturn).then_some(op)
    }

    /// The VIR signature the extern must have (no allocation: backends and passes ask for it
    /// at every call of an extern).
    pub fn signature(self) -> (&'static [Ty], Ty) {
        use Ty::{F32, F64, I32, I64, U16, U32, U64};
        match self {
            MathOp::Sqrt
            | MathOp::Floor
            | MathOp::Ceil
            | MathOp::Round
            | MathOp::Trunc
            | MathOp::Abs => (&[F64], F64),
            MathOp::PowF64 => (&[F64, F64], F64),
            MathOp::PowI64 => (&[I64, I64], I64),
            MathOp::F64Bits => (&[F64], U64),
            MathOp::BitsF64 => (&[U64], F64),
            MathOp::Umulh => (&[U64, U64], U64),
            MathOp::ToInt32 => (&[F64], I32),
            MathOp::MulInt32 => (&[I32, I32], I32),
            MathOp::AddInt32 => (&[I32, F64], I32),
            MathOp::Clz32 => (&[I32], I32),
            MathOp::Clz64 | MathOp::Ctz64 | MathOp::Popcount64 => (&[U64], U64),
            MathOp::F32Bits => (&[F32], U32),
            MathOp::BitsF32 => (&[U32], F32),
            MathOp::F16ToF32 | MathOp::Bf16ToF32 => (&[U16], F32),
            MathOp::F32ToF16 | MathOp::F32ToBf16 => (&[F32], U16),
            MathOp::F64ToF16 | MathOp::F64ToBf16 => (&[F64], U16),
            MathOp::I64ToBf16 => (&[I64], U16),
            MathOp::U64ToBf16 => (&[U64], U16),
            MathOp::F16Bits | MathOp::BitsF16 | MathOp::Bf16Bits | MathOp::BitsBf16 => {
                (&[U16], U16)
            }
        }
    }

    /// The rounding `Math.floor`, `ceil`, `trunc` or `round` does, if this is one of them.
    pub fn rounding(self) -> Option<Rounding> {
        Some(match self {
            MathOp::Floor => Rounding::Floor,
            MathOp::Ceil => Rounding::Ceil,
            MathOp::Trunc => Rounding::Trunc,
            MathOp::Round => Rounding::Round,
            _ => return None,
        })
    }
}

impl MathOp {
    /// The operation on constants, for folding: each argument and the result as the bits of its
    /// type ([`MathOp::signature`]) zero-extended to 64 (an `i32` is its 32-bit pattern, an
    /// `f32` and an `f64` their IEEE 754 bits). Computes exactly what velt_rt's function does
    /// (a test of veltc compares the two); for a `velt_math_*` operation, which has no runtime
    /// function, this is the reference. `None` where folding could differ from running: `pow` on
    /// doubles (the runtime's fdlibm), and a NaN argument or result of a double operation (a
    /// NaN's bits depend on the instruction that makes it).
    pub fn eval(self, args: &[u64]) -> Option<u64> {
        let f = |i: usize| args.get(i).map(|&b| f64::from_bits(b));
        let i32_ = |i: usize| args.get(i).map(|&b| b as u32 as i32);
        let u = |i: usize| args.get(i).copied();
        let double = |x: f64| (!x.is_nan()).then(|| x.to_bits());
        let int32 = |x: i32| Some(u64::from(x as u32));
        if args.len() != self.signature().0.len() {
            return None;
        }
        match self {
            MathOp::Sqrt
            | MathOp::Floor
            | MathOp::Ceil
            | MathOp::Round
            | MathOp::Trunc
            | MathOp::Abs => {
                let x = f(0).filter(|x| !x.is_nan())?;
                double(match self {
                    MathOp::Sqrt => x.sqrt(),
                    MathOp::Floor => x.floor(),
                    MathOp::Ceil => x.ceil(),
                    MathOp::Trunc => x.trunc(),
                    MathOp::Abs => x.abs(),
                    _ => js_round(x),
                })
            }
            MathOp::PowF64 => None,
            MathOp::PowI64 => Some(pow_i64(u(0)? as i64, u(1)? as i64) as u64),
            MathOp::F64Bits => u(0).filter(|&b| !f64::from_bits(b).is_nan()),
            MathOp::BitsF64 => double(f64::from_bits(u(0)?)),
            MathOp::Umulh => Some(((u128::from(u(0)?) * u128::from(u(1)?)) >> 64) as u64),
            MathOp::ToInt32 => int32(to_int32(f(0).filter(|x| !x.is_nan())?)),
            MathOp::MulInt32 => {
                let p = i64::from(i32_(0)?) * i64::from(i32_(1)?);
                int32((p as f64) as i64 as i32)
            }
            MathOp::AddInt32 => {
                let x = f(1).filter(|x| !x.is_nan())?;
                int32(to_int32(f64::from(i32_(0)?) + x))
            }
            MathOp::Clz32 => int32(i32_(0)?.leading_zeros() as i32),
            MathOp::Clz64 => Some(u64::from(u(0)?.leading_zeros())),
            MathOp::Ctz64 => Some(u64::from(u(0)?.trailing_zeros())),
            MathOp::Popcount64 => Some(u64::from(u(0)?.count_ones())),
            // A bit cast: the same 32 bits, unless they are a NaN's (as `F64Bits`).
            MathOp::F32Bits | MathOp::BitsF32 => {
                u(0).filter(|&b| !f32::from_bits(b as u32).is_nan() && b >> 32 == 0)
            }
            // The f16/bf16 conversions: a NaN in or out is not folded (as for doubles).
            MathOp::F16ToF32 | MathOp::Bf16ToF32 => {
                let b = u(0).filter(|&b| b >> 16 == 0)? as u16;
                let r = if self == MathOp::F16ToF32 {
                    half::f16_to_f32(b)
                } else {
                    half::bf16_to_f32(b)
                };
                (!f32::from_bits(r).is_nan()).then_some(u64::from(r))
            }
            MathOp::F32ToF16 | MathOp::F32ToBf16 => {
                let b = u(0).filter(|&b| b >> 32 == 0)? as u32;
                (!f32::from_bits(b).is_nan()).then_some(u64::from(if self == MathOp::F32ToF16 {
                    half::f32_to_f16(b)
                } else {
                    half::f32_to_bf16(b)
                }))
            }
            MathOp::F64ToF16 | MathOp::F64ToBf16 => {
                let b = u(0).filter(|&b| !f64::from_bits(b).is_nan())?;
                Some(u64::from(if self == MathOp::F64ToF16 {
                    half::f64_to_f16(b)
                } else {
                    half::f64_to_bf16(b)
                }))
            }
            MathOp::I64ToBf16 => Some(u64::from(half::i64_to_bf16(u(0)? as i64))),
            MathOp::U64ToBf16 => Some(u64::from(half::u64_to_bf16(u(0)?))),
            // The value of an `f16`/`bf16` is its bits: these name the reinterpretation.
            MathOp::F16Bits | MathOp::BitsF16 | MathOp::Bf16Bits | MathOp::BitsBf16 => {
                u(0).filter(|&b| b >> 16 == 0)
            }
        }
    }
}

/// JS's `Math.round` (velt_rt's `velt_rt_math_round`).
fn js_round(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let f = x.floor();
    let r = if x - f >= 0.5 { f + 1.0 } else { f };
    if r == 0.0 && x.is_sign_negative() {
        -0.0
    } else {
        r
    }
}

/// JS's ToInt32 (velt_rt's `velt_rt_math_to_int32`).
fn to_int32(x: f64) -> i32 {
    if !x.is_finite() {
        return 0;
    }
    if x.abs() < 9_223_372_036_854_775_808.0 {
        return x as i64 as i32;
    }
    let bits = x.to_bits();
    let shift = ((bits >> 52) & 0x7ff) as i64 - 1075;
    let mantissa = (bits & ((1 << 52) - 1)) | (1 << 52);
    let low = if shift >= 32 {
        0
    } else {
        (mantissa << shift) as u32
    };
    let low = if x < 0.0 { low.wrapping_neg() } else { low };
    low as i32
}

/// Wrapping integer power (velt_rt's `velt_rt_pow_i64`).
fn pow_i64(a: i64, b: i64) -> i64 {
    if b < 0 {
        return if a == 1 { 1 } else { 0 };
    }
    let (mut base, mut exp, mut acc) = (a, b as u64, 1i64);
    while exp > 0 {
        if exp & 1 == 1 {
            acc = acc.wrapping_mul(base);
        }
        exp >>= 1;
        if exp > 0 {
            base = base.wrapping_mul(base);
        }
    }
    acc
}

/// How `Math.floor`, `ceil`, `trunc` and `round` round a double to a whole one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rounding {
    /// Toward -Infinity.
    Floor,
    /// Toward +Infinity.
    Ceil,
    /// Toward 0.
    Trunc,
    /// To the nearest, ties toward +Infinity (JS's `Math.round`).
    Round,
}

/// What a runtime function may do to memory, beyond its arguments and result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effects {
    /// Anything (every runtime function not listed in [`effects`]).
    Any,
    /// Reads memory, writes none (string comparison, hashing).
    ReadOnly,
    /// Reads memory and writes only state of its own that compiled code never reads: a string's
    /// position-translation table and lone-surrogate count, built on first use (#377 phase 2b:
    /// `charCodeAt` on non-ASCII text, `indexOf`).
    ReadCaching,
    /// Touches no memory: every [`MathOp`].
    None,
    /// Does nothing; a call marks the way it is on as rare (`velt_rt_array_view_cold`).
    Cold,
}

impl Effects {
    /// Whether the function writes no memory compiled code reads (so values loaded before a
    /// call are still valid after it, and it frees nothing).
    pub fn writes_nothing_visible(self) -> bool {
        matches!(
            self,
            Effects::None | Effects::ReadOnly | Effects::ReadCaching
        )
    }
}

/// The runtime functions besides the [`MathOp`]s whose effects are not [`Effects::Any`].
const EFFECTS: [(&str, Effects); 10] = [
    ("velt_rt_str_cmp", Effects::ReadOnly),
    ("velt_rt_str_eq", Effects::ReadOnly),
    ("velt_rt_str_hash", Effects::ReadOnly),
    ("velt_rt_str_starts_with", Effects::ReadOnly),
    ("velt_rt_str_ends_with", Effects::ReadOnly),
    ("velt_rt_str_char_code_at", Effects::ReadCaching),
    ("velt_rt_str_index_of", Effects::ReadCaching),
    ("velt_rt_str_last_index_of", Effects::ReadCaching),
    ("velt_rt_str_includes", Effects::ReadCaching),
    ("velt_rt_array_view_cold", Effects::Cold),
];

/// What the runtime function `symbol` may do to memory.
pub fn effects(symbol: &str) -> Effects {
    if MathOp::of_symbol(symbol).is_some() {
        return Effects::None;
    }
    EFFECTS
        .iter()
        .find(|(s, _)| *s == symbol)
        .map_or(Effects::Any, |&(_, e)| e)
}

/// Every runtime function [`effects`] knows more of than [`Effects::Any`] (the runtime must
/// define each: a test of velt_opt checks). The `velt_math_*` operations are not runtime
/// functions, so they are not listed.
pub fn with_effects() -> impl Iterator<Item = &'static str> {
    let ops = MathOp::ALL
        .into_iter()
        .filter(|op| op.has_runtime_function());
    ops.map(MathOp::symbol)
        .chain(EFFECTS.iter().map(|&(s, _)| s))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every operation's symbol names it and no other.
    #[test]
    fn symbols_round_trip() {
        for op in MathOp::ALL {
            assert_eq!(MathOp::of_symbol(op.symbol()), Some(op));
        }
        assert_eq!(MathOp::of_symbol("velt_rt_math_sin"), None);
    }

    /// An extern is its operation only with the operation's signature.
    #[test]
    fn of_extern_checks_the_signature() {
        let ext = |params: Vec<Ty>, ret| ExternFn {
            symbol: "velt_rt_math_clz64".into(),
            params,
            ret,
            noreturn: false,
            host: None,
        };
        assert_eq!(
            MathOp::of_extern(&ext(vec![Ty::U64], Ty::U64)),
            Some(MathOp::Clz64)
        );
        assert_eq!(MathOp::of_extern(&ext(vec![Ty::I64], Ty::U64)), None);
        assert_eq!(MathOp::of_extern(&ext(vec![Ty::U64], Ty::U32)), None);
    }

    /// The `velt_math_*` operations have no runtime function; the `velt_rt_*` ones do.
    #[test]
    fn runtime_functions() {
        assert!(MathOp::Sqrt.has_runtime_function());
        assert!(MathOp::PowF64.has_runtime_function());
        assert!(!MathOp::F32Bits.has_runtime_function());
        assert!(!MathOp::BitsF32.has_runtime_function());
        assert_eq!(MathOp::F32Bits.name(), "f32_bits");
        assert_eq!(MathOp::BitsF64.name(), "bits_f64");
    }

    /// The f32 bit casts fold to the same 32 bits, except a NaN's (whose bits the hardware may
    /// change) and an argument wider than 32 bits (no valid operand is).
    #[test]
    fn f32_bit_casts_fold() {
        for x in [
            0.0f32,
            -0.0,
            1.0,
            -1.5,
            0.1,
            f32::MIN_POSITIVE,
            1e-45,
            f32::MAX,
            f32::INFINITY,
        ] {
            let b = u64::from(x.to_bits());
            assert_eq!(MathOp::F32Bits.eval(&[b]), Some(b), "{x}");
            assert_eq!(MathOp::BitsF32.eval(&[b]), Some(b), "{x}");
        }
        for nan in [0x7fc0_0000u64, 0x7f80_0001, 0xffff_ffff] {
            assert_eq!(MathOp::F32Bits.eval(&[nan]), None);
            assert_eq!(MathOp::BitsF32.eval(&[nan]), None);
        }
        assert_eq!(MathOp::BitsF32.eval(&[1 << 32]), None);
        assert_eq!(MathOp::F32Bits.eval(&[]), None);
    }

    #[test]
    fn effects_of_runtime_functions() {
        for op in MathOp::ALL {
            assert_eq!(effects(op.symbol()), Effects::None, "{op:?}");
        }
        assert_eq!(effects("velt_rt_str_cmp"), Effects::ReadOnly);
        assert_eq!(effects("velt_rt_str_index_of"), Effects::ReadCaching);
        assert_eq!(effects("velt_rt_array_view_cold"), Effects::Cold);
        assert_eq!(effects("velt_rt_write_str"), Effects::Any);
        assert!(!Effects::Cold.writes_nothing_visible());
        assert!(Effects::ReadCaching.writes_nothing_visible());
    }
}
