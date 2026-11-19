//! The 256-bit vector types (`f32x8`, #3140): what each `velt_simd_<type>_<op>` extern of one
//! means. VIR has no 256-bit type. A wide vector is a value of two 128-bit halves in memory,
//! the low lanes first (32 bytes, aligned as a `V128` is: to 8), and its operations take it by
//! address: every vector operand of the 128-bit operation is a `Ptr` to such a value, and a vector
//! result is written through one more `Ptr`, the last parameter.
//!
//! A wide operation reads the 32 bytes at each operand pointer (and what the 128-bit operation
//! reads: `load` its array), writes the 32 bytes at its result pointer (`store` its array
//! instead), and does nothing else: it keeps no pointer, cannot unwind and touches no other
//! memory. Lowering gives every result a slot of its own, so the result pointer never equals an
//! operand's; backends read all operands before they write all the same.
//!
//! Each lane is the 128-bit operation's lane of the matching half, on every target: a backend
//! with 256-bit registers computes both halves in one instruction, and any other the two
//! halves in turn.

use super::{Lanes, Op};
use crate::vir::Ty;

/// The lanes of the halves of the wide type `name` (`f32x8` is two `f32x4`).
pub(super) fn named(name: &str) -> Option<Lanes> {
    WIDE.into_iter().find(|l| self::name(*l) == name)
}

/// The lane types that have a wide form.
pub const WIDE: [Lanes; 1] = [Lanes::F32];

/// The Velt name of the wide type with halves of `lanes`: `f32x8`.
pub fn name(lanes: Lanes) -> String {
    format!("{}x{}", lanes.scalar(), 2 * lanes.count())
}

/// Whether the wide type with halves of `lanes` has `op`: the lane-by-lane operations a kernel
/// needs. The rest of the API (`fits`, `sum`, `lane`, the halves) is std code on the halves.
pub(super) fn supported(lanes: Lanes, op: Op) -> bool {
    lanes == Lanes::F32
        && matches!(
            op,
            Op::Splat
                | Op::Load
                | Op::Store
                | Op::Add
                | Op::Sub
                | Op::Mul
                | Op::Div
                | Op::Min
                | Op::Max
                | Op::Neg
                | Op::Abs
                | Op::Sqrt
                | Op::MulAdd
                | Op::MulAddFast
        )
}

/// The VIR signature of the wide form of an operation whose 128-bit form has `narrow`: each
/// vector a `Ptr`, a vector result a last `Ptr` parameter.
pub(super) fn signature(narrow: (Vec<Ty>, Ty)) -> (Vec<Ty>, Ty) {
    let (params, ret) = narrow;
    let by_address = |t: Ty| if t == Ty::V128 { Ty::Ptr } else { t };
    let mut params: Vec<Ty> = params.into_iter().map(by_address).collect();
    if ret == Ty::V128 {
        params.push(Ty::Ptr);
        return (params, Ty::Unit);
    }
    (params, ret)
}

/// Which parameters of the wide form of `op` are wide vectors to read: their indices.
pub fn operands(op: Op) -> std::ops::Range<usize> {
    match op {
        Op::Splat | Op::Load => 0..0,
        Op::Store | Op::Neg | Op::Abs | Op::Sqrt => 0..1,
        Op::MulAdd | Op::MulAddFast => 0..3,
        _ => 0..2,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Lanes, Op, SimdCall};
    use crate::vir::Ty::{Ptr, Unit, F32, U64};

    #[test]
    fn wide_operations_take_their_vectors_by_address() {
        let of = |op: &str| SimdCall::of_symbol(&format!("velt_simd_f32x8_{op}"));
        let sig = |op: &str| of(op).unwrap().signature();
        assert_eq!(of("add"), Some(SimdCall::Wide(Lanes::F32, Op::Add)));
        assert_eq!(sig("add"), (vec![Ptr, Ptr, Ptr], Unit));
        assert_eq!(sig("sqrt"), (vec![Ptr, Ptr], Unit));
        assert_eq!(sig("mul_add_fast"), (vec![Ptr, Ptr, Ptr, Ptr], Unit));
        assert_eq!(sig("splat"), (vec![F32, Ptr], Unit));
        assert_eq!(sig("load"), (vec![Ptr, U64, Ptr], Unit));
        assert_eq!(sig("store"), (vec![Ptr, Ptr, U64], Unit));
        // What std computes on the halves has no wide operation, nor has another lane type.
        for op in [
            "fits",
            "sum",
            "lane",
            "eq",
            "select",
            "zip_low",
            "load_partial",
        ] {
            assert_eq!(of(op), None, "{op}");
        }
        assert_eq!(SimdCall::of_symbol("velt_simd_f64x4_add"), None);
        assert_eq!(SimdCall::of_symbol("velt_simd_i32x8_add"), None);
        assert_eq!(super::name(Lanes::F32), "f32x8");
        // The operands a backend reads: the vectors, which come first.
        assert_eq!(super::operands(Op::Splat), 0..0);
        assert_eq!(super::operands(Op::Load), 0..0);
        assert_eq!(super::operands(Op::Store), 0..1);
        assert_eq!(super::operands(Op::Neg), 0..1);
        assert_eq!(super::operands(Op::Max), 0..2);
        assert_eq!(super::operands(Op::MulAdd), 0..3);
    }
}
