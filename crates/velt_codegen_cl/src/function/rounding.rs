//! `Math.floor`, `Math.ceil`, `Math.trunc` and `Math.round` inline (#535).
//!
//! Cranelift's `floor`/`ceil`/`trunc` are one instruction on aarch64 and on x86-64 with SSE4.1;
//! on baseline x86-64 (any target given as a triple, which gets no host CPU features) they
//! become calls into the C runtime. There they round through a conversion to `i64` instead:
//! for |x| < 2^52 truncating to an integer and back is exact, a comparison with `x` corrects
//! the direction, and `fcopysign` restores the sign of a zero result (`Math.ceil(-0.5)` is
//! `-0`). Larger values, infinities and NaN are already integral and are returned as they are.
//!
//! `Math.round` (ties toward +Infinity) is `floor` plus one comparison: `x - floor(x)` is exact
//! for every finite double, and `fcopysign` gives `-0` for inputs in `[-0.5, -0]`, as in JS.

use cranelift_codegen::ir::condcodes::FloatCC;
use cranelift_codegen::ir::{self, types::F64, types::I64, InstBuilder};
use cranelift_codegen::isa::TargetIsa;
use cranelift_module::Module;
use target_lexicon::Architecture;

use super::Translator;

/// A rounding direction of the `Math` functions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Rounding {
    Floor,
    Ceil,
    Trunc,
    /// JS `Math.round`: to the nearest integer, ties toward +Infinity.
    Round,
}

impl Rounding {
    /// The rounding done by the runtime function `symbol`, if it is one.
    pub(super) fn of_symbol(symbol: &str) -> Option<Rounding> {
        Some(match symbol {
            "velt_rt_math_floor" => Rounding::Floor,
            "velt_rt_math_ceil" => Rounding::Ceil,
            "velt_rt_math_trunc" => Rounding::Trunc,
            "velt_rt_math_round" => Rounding::Round,
            _ => return None,
        })
    }
}

impl<M: Module> Translator<'_, '_, M> {
    /// `x` (an `f64`) rounded as `how` says.
    pub(super) fn round_f64(&mut self, x: ir::Value, how: Rounding) -> ir::Value {
        if how == Rounding::Round {
            return self.js_round(x);
        }
        if self.rounding_instructions {
            let ins = self.builder.ins();
            return match how {
                Rounding::Floor => ins.floor(x),
                Rounding::Ceil => ins.ceil(x),
                _ => ins.trunc(x),
            };
        }
        self.round_by_conversion(x, how)
    }

    fn round_by_conversion(&mut self, x: ir::Value, how: Rounding) -> ir::Value {
        let b = &mut self.builder;
        let a = b.ins().fabs(x);
        let limit = b.ins().f64const(4_503_599_627_370_496.0);
        let small = b.ins().fcmp(FloatCC::LessThan, a, limit);
        let i = b.ins().fcvt_to_sint_sat(I64, x);
        let t = b.ins().fcvt_from_sint(F64, i);
        let step = match how {
            Rounding::Floor => Some((FloatCC::GreaterThan, -1.0)),
            Rounding::Ceil => Some((FloatCC::LessThan, 1.0)),
            _ => None,
        };
        let r = match step {
            Some((cc, one)) => {
                let off = b.ins().fcmp(cc, t, x);
                let one = b.ins().f64const(one);
                let zero = b.ins().f64const(0.0);
                let adj = b.ins().select(off, one, zero);
                b.ins().fadd(t, adj)
            }
            None => t,
        };
        let signed = b.ins().fcopysign(r, x);
        b.ins().select(small, signed, x)
    }

    fn js_round(&mut self, x: ir::Value) -> ir::Value {
        let f = self.round_f64(x, Rounding::Floor);
        let b = &mut self.builder;
        let d = b.ins().fsub(x, f);
        let half = b.ins().f64const(0.5);
        let up = b.ins().fcmp(FloatCC::GreaterThanOrEqual, d, half);
        let one = b.ins().f64const(1.0);
        let f1 = b.ins().fadd(f, one);
        let r = b.ins().select(up, f1, f);
        b.ins().fcopysign(r, x)
    }
}

/// Whether `isa` has `floor`/`ceil`/`trunc` instructions: everything but x86-64 without SSE4.1.
pub(super) fn has_rounding_instructions(isa: &dyn TargetIsa) -> bool {
    if !matches!(isa.triple().architecture, Architecture::X86_64) {
        return true;
    }
    isa.isa_flags()
        .iter()
        .any(|f| f.name == "has_sse41" && f.as_bool() == Some(true))
}
