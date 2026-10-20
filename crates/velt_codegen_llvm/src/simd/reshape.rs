//! The helpers that give lanes of another shape: interleaving two vectors, widening half a
//! vector, narrowing two, converting between integer and float lanes.

use velt_vir::simd::Lanes;

use super::{spell, Body};

/// The lanes of the low (or high) halves of `%a0` and `%a1` in turn.
pub(super) fn zip(b: &mut Body, lanes: Lanes, high: bool) -> String {
    let (_, ty, _) = spell(lanes);
    let (x, y) = (b.arg(0, &ty), b.arg(1, &ty));
    let n = lanes.count();
    let first = if high { n / 2 } else { 0 };
    let picks: Vec<u32> = (first..first + n / 2).flat_map(|k| [k, n + k]).collect();
    b.inst(format!(
        "shufflevector {ty} {x}, {ty} {y}, {}",
        indices(&picks)
    ))
}

/// A constant vector of `i32` shuffle indices.
fn indices(picks: &[u32]) -> String {
    let list: Vec<String> = picks.iter().map(|k| format!("i32 {k}")).collect();
    format!("<{} x i32> <{}>", picks.len(), list.join(", "))
}

/// The low (or high) half's lanes of `%a0` at twice the width.
pub(super) fn widen(b: &mut Body, lanes: Lanes, high: bool) -> String {
    let (lane, ty, _) = spell(lanes);
    let wider = lanes
        .wider()
        .expect("ICE: widen of lanes that have no wider ones");
    let (_, wide, _) = spell(wider);
    let half = lanes.count() / 2;
    let x = b.arg(0, &ty);
    let first = if high { half } else { 0 };
    let picks: Vec<u32> = (first..first + half).collect();
    let part = b.inst(format!(
        "shufflevector {ty} {x}, {ty} poison, {}",
        indices(&picks)
    ));
    let extend = if lanes.is_signed() { "sext" } else { "zext" };
    let extended = b.inst(format!("{extend} <{half} x {lane}> {part} to {wide}"));
    b.bytes(&extended, &wide)
}

/// The lanes of `%a0`, then of `%a1`, clamped to the lanes half as wide.
pub(super) fn narrow(b: &mut Body, lanes: Lanes) -> String {
    let (lane, ty, _) = spell(lanes);
    let to = lanes
        .narrower()
        .expect("ICE: narrow of lanes that have no narrower ones");
    let (_, narrow, _) = spell(to);
    let n = lanes.count();
    let both = format!("<{} x {lane}>", 2 * n);
    let suffix = format!("v{}i{}", 2 * n, lanes.bits());
    let (x, y) = (b.arg(0, &ty), b.arg(1, &ty));
    let picks: Vec<u32> = (0..2 * n).collect();
    let all = b.inst(format!(
        "shufflevector {ty} {x}, {ty} {y}, {}",
        indices(&picks)
    ));
    let splat = |v: i128| vec![format!("{lane} {v}"); 2 * n as usize].join(", ");
    let half = to.bits();
    let clamped = if lanes.is_signed() {
        let (low, high) = (-(1i128 << (half - 1)), (1i128 << (half - 1)) - 1);
        let floor = b.intrinsic(
            "smax",
            &suffix,
            &both,
            &[&all, &format!("<{}>", splat(low))],
            "",
        );
        b.intrinsic(
            "smin",
            &suffix,
            &both,
            &[&floor, &format!("<{}>", splat(high))],
            "",
        )
    } else {
        let high = (1i128 << half) - 1;
        b.intrinsic(
            "umin",
            &suffix,
            &both,
            &[&all, &format!("<{}>", splat(high))],
            "",
        )
    };
    let cut = b.inst(format!("trunc {both} {clamped} to {narrow}"));
    b.bytes(&cut, &narrow)
}

/// Every lane of `%a0` converted to the lanes `to`: integers to floats rounded to nearest,
/// floats to integers truncated and clamped (NaN is 0).
pub(super) fn convert(b: &mut Body, lanes: Lanes, to: Lanes) -> String {
    let (_, from_ty, from_suffix) = spell(lanes);
    let (_, to_ty, to_suffix) = spell(to);
    let x = b.arg(0, &from_ty);
    let value = if to.is_float() {
        let cast = if lanes.is_signed() {
            "sitofp"
        } else {
            "uitofp"
        };
        b.inst(format!("{cast} {from_ty} {x} to {to_ty}"))
    } else {
        let name = if to.is_signed() { "fptosi" } else { "fptoui" };
        let intrinsic = format!("llvm.{name}.sat.{to_suffix}.{from_suffix}");
        b.declarations
            .push(format!("declare {to_ty} @{intrinsic}({from_ty})"));
        b.inst(format!("call {to_ty} @{intrinsic}({from_ty} {x})"))
    };
    b.bytes(&value, &to_ty)
}
