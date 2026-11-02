//! `Rvalue::Intrinsic`'s operations (vir.rs invariant 12, #1981).

use super::Ty;

/// A pure operation `Rvalue::Intrinsic` computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Intrinsic {
    /// A scalar math operation of the runtime (`crate::math`), computed inline or through its
    /// runtime function, which backends call as an ordinary function.
    Math(crate::math::MathOp),
}

impl Intrinsic {
    /// The operand types and the result type.
    pub fn signature(self) -> (&'static [Ty], Ty) {
        match self {
            Intrinsic::Math(op) => op.signature(),
        }
    }

    /// Its name in debug dumps (`math.sqrt`).
    pub fn name(self) -> String {
        match self {
            Intrinsic::Math(op) => format!("math.{}", op.name()),
        }
    }
}
