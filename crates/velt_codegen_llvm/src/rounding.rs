//! `Math.floor`, `Math.ceil`, `Math.trunc` and `Math.round` as inline code (#535).
//!
//! On aarch64 and WebAssembly `llvm.floor`/`llvm.ceil`/`llvm.trunc` are one instruction. The
//! baseline x86-64 CPU that release builds target has no SSE4.1 `roundsd`, so there they would
//! be calls into the C runtime; instead `@velt.floor` & co. round through a conversion to `i64`:
//! for |x| < 2^52 truncating to an integer and back is exact, a comparison with `x` corrects
//! the direction, and `copysign` restores the sign of a zero result (`Math.ceil(-0.5)` is `-0`).
//! Larger values, infinities and NaN are already integral and are returned as they are. The
//! out-of-range conversion is poison only on the path the final `select` discards.
//!
//! `Math.round` (ties toward +Infinity) is `floor` plus one comparison: `x - floor(x)` is exact
//! for every finite double, and `copysign` gives `-0` for inputs in `[-0.5, -0]`, as in JS.

/// The helpers' shared declarations (each one line, so equal lines from different helpers
/// coincide in the module's set of definitions).
const FABS: &str = "declare double @llvm.fabs.f64(double)";
const COPYSIGN: &str = "declare double @llvm.copysign.f64(double, double)";
const FLOOR_INTRINSIC: &str = "declare double @llvm.floor.f64(double)";

const FLOOR: &str = "define internal double @velt.floor(double %x) alwaysinline nounwind {
  %a = call double @llvm.fabs.f64(double %x)
  %small = fcmp olt double %a, 0x4330000000000000
  %i = fptosi double %x to i64
  %t = sitofp i64 %i to double
  %over = fcmp ogt double %t, %x
  %adj = select i1 %over, double 1.0, double 0.0
  %f = fsub double %t, %adj
  %s = call double @llvm.copysign.f64(double %f, double %x)
  %r = select i1 %small, double %s, double %x
  ret double %r
}";

const CEIL: &str = "define internal double @velt.ceil(double %x) alwaysinline nounwind {
  %a = call double @llvm.fabs.f64(double %x)
  %small = fcmp olt double %a, 0x4330000000000000
  %i = fptosi double %x to i64
  %t = sitofp i64 %i to double
  %under = fcmp olt double %t, %x
  %adj = select i1 %under, double 1.0, double 0.0
  %f = fadd double %t, %adj
  %s = call double @llvm.copysign.f64(double %f, double %x)
  %r = select i1 %small, double %s, double %x
  ret double %r
}";

const TRUNC: &str = "define internal double @velt.trunc(double %x) alwaysinline nounwind {
  %a = call double @llvm.fabs.f64(double %x)
  %small = fcmp olt double %a, 0x4330000000000000
  %i = fptosi double %x to i64
  %t = sitofp i64 %i to double
  %s = call double @llvm.copysign.f64(double %t, double %x)
  %r = select i1 %small, double %s, double %x
  ret double %r
}";

/// `Math.round` over `@velt.floor` (x86-64).
const ROUND_BY_CONVERSION: &str =
    "define internal double @velt.round(double %x) alwaysinline nounwind {
  %f = call double @velt.floor(double %x)
  %d = fsub double %x, %f
  %up = fcmp oge double %d, 0.5
  %f1 = fadd double %f, 1.0
  %r = select i1 %up, double %f1, double %f
  %s = call double @llvm.copysign.f64(double %r, double %x)
  ret double %s
}";

/// `Math.round` over `llvm.floor` (one instruction on the other targets).
const ROUND: &str = "define internal double @velt.round(double %x) alwaysinline nounwind {
  %f = call double @llvm.floor.f64(double %x)
  %d = fsub double %x, %f
  %up = fcmp oge double %d, 0.5
  %f1 = fadd double %f, 1.0
  %r = select i1 %up, double %f1, double %f
  %s = call double @llvm.copysign.f64(double %r, double %x)
  ret double %s
}";

/// The helper that computes the runtime function `symbol` of type `(f64) -> f64` inline, with
/// the definitions it needs: `Math.round` on every target, `floor`/`ceil`/`trunc` where they
/// round by conversion (`by_conversion`, x86-64; elsewhere they are LLVM intrinsics,
/// `runtime::math_intrinsic`).
pub(crate) fn helper(
    symbol: &str,
    by_conversion: bool,
) -> Option<(&'static str, &'static [&'static str])> {
    Some(match (symbol, by_conversion) {
        ("velt_rt_math_round", true) => {
            ("@velt.round", &[FABS, COPYSIGN, FLOOR, ROUND_BY_CONVERSION])
        }
        ("velt_rt_math_round", false) => ("@velt.round", &[FLOOR_INTRINSIC, COPYSIGN, ROUND]),
        ("velt_rt_math_floor", true) => ("@velt.floor", &[FABS, COPYSIGN, FLOOR]),
        ("velt_rt_math_ceil", true) => ("@velt.ceil", &[FABS, COPYSIGN, CEIL]),
        ("velt_rt_math_trunc", true) => ("@velt.trunc", &[FABS, COPYSIGN, TRUNC]),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_by_target() {
        assert_eq!(helper("velt_rt_math_floor", true).unwrap().0, "@velt.floor");
        assert!(helper("velt_rt_math_floor", false).is_none());
        assert!(helper("velt_rt_math_sqrt", true).is_none());
        let (_, defs) = helper("velt_rt_math_round", false).unwrap();
        assert!(defs.contains(&FLOOR_INTRINSIC) && !defs.contains(&FLOOR));
        let (_, defs) = helper("velt_rt_math_round", true).unwrap();
        assert!(defs.contains(&FLOOR) && !defs.contains(&FLOOR_INTRINSIC));
    }
}
