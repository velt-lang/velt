//! The scalar math operations: what each `velt_rt_math_*` (and `velt_rt_pow_*`) extern means
//! (rt_abi.md "Math"), and what every runtime function may do to memory ([`Effects`]).
//!
//! Unlike the SIMD and atomic externs, these are real runtime functions (velt_rt's `math.rs`),
//! which are the reference for what each computes; backends emit most of them inline (one
//! instruction, or a short sequence with the runtime call on a cold path) and optimizers treat
//! them as arithmetic. This module is the one place that says which symbols those are, with
//! their signatures: backends and passes match on [`MathOp`], not on symbol strings, and the
//! verifier rejects a `velt_rt_math_*` extern whose signature is not its operation's.

use crate::vir::{ExternFn, Ty};

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
}

impl MathOp {
    /// Every operation.
    pub const ALL: [MathOp; 18] = [
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
        }
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
        (e.params == params && e.ret == ret && !e.noreturn).then_some(op)
    }

    /// The VIR signature the extern must have.
    pub fn signature(self) -> (Vec<Ty>, Ty) {
        use Ty::{F64, I32, I64, U64};
        match self {
            MathOp::Sqrt
            | MathOp::Floor
            | MathOp::Ceil
            | MathOp::Round
            | MathOp::Trunc
            | MathOp::Abs => (vec![F64], F64),
            MathOp::PowF64 => (vec![F64, F64], F64),
            MathOp::PowI64 => (vec![I64, I64], I64),
            MathOp::F64Bits => (vec![F64], U64),
            MathOp::BitsF64 => (vec![U64], F64),
            MathOp::Umulh => (vec![U64, U64], U64),
            MathOp::ToInt32 => (vec![F64], I32),
            MathOp::MulInt32 => (vec![I32, I32], I32),
            MathOp::AddInt32 => (vec![I32, F64], I32),
            MathOp::Clz32 => (vec![I32], I32),
            MathOp::Clz64 | MathOp::Ctz64 | MathOp::Popcount64 => (vec![U64], U64),
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
/// define each: a test of velt_opt checks).
pub fn with_effects() -> impl Iterator<Item = &'static str> {
    (MathOp::ALL.into_iter().map(MathOp::symbol)).chain(EFFECTS.iter().map(|&(s, _)| s))
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

    #[test]
    fn effects_of_runtime_functions() {
        for op in MathOp::ALL {
            assert_eq!(effects(op.symbol()), Effects::None, "{op:?}");
        }
        assert_eq!(effects("velt_rt_str_cmp"), Effects::ReadOnly);
        assert_eq!(effects("velt_rt_str_index_of"), Effects::ReadCaching);
        assert_eq!(effects("velt_rt_array_view_cold"), Effects::Cold);
        assert_eq!(effects("velt_rt_write_i64"), Effects::Any);
        assert!(!Effects::Cold.writes_nothing_visible());
        assert!(Effects::ReadCaching.writes_nothing_visible());
    }
}
