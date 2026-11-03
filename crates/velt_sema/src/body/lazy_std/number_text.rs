//! The hooks of std's number printing, checked on demand (velt_common `std_hooks::ON_DEMAND_TEXT`,
//! `ON_DEMAND_FLOAT`): reached once the program's own code converts to text a value that needs
//! them. `texts` are the types of the values it converts (`ToString`, `TemplateToString`) in the
//! code scanned since the last call, std's own excepted (the prelude is checked whatever the
//! program uses, and its conversions would make every program check the hooks).

use crate::ctx::Ctx;
use crate::hir::{self, DefId, TyId, TyKind};

/// Adds the on-demand hooks that `texts` now need to `found`.
pub(super) fn reach_hooks(cx: &mut Ctx, texts: Vec<TyId>, found: &mut Vec<DefId>) {
    let number = !cx.lazy.number_text && texts.iter().any(|&t| is_number_text(cx, t));
    let float = !cx.lazy.float_text && texts.iter().any(|&t| may_hold_float(cx, t));
    for (now, names) in [
        (number, velt_common::std_hooks::ON_DEMAND_TEXT),
        (float, velt_common::std_hooks::ON_DEMAND_FLOAT),
    ] {
        if !now {
            continue;
        }
        for q in velt_common::std_hooks::on_demand(names) {
            if let Some(crate::ctx::Item::Def(d)) = cx.std_def(&q) {
                found.push(d);
            }
        }
    }
    cx.lazy.number_text |= number;
    cx.lazy.float_text |= float;
}

/// May the text of a value of type `t` need std's integer printing: is it anything but a
/// string or a boolean?
fn is_number_text(cx: &Ctx, t: TyId) -> bool {
    !matches!(
        cx.ty.kind(t),
        TyKind::Bool
            | TyKind::Str
            | TyKind::Literal(hir::LitValue::Bool(_) | hir::LitValue::Str(_))
    )
}

/// Can a value of type `t` hold a float, so that its text may need std's float printing? Only
/// integers, booleans and strings are known not to; anything else (an object, an array, `any`,
/// a type parameter) may, and is printed through glue that can reach a float.
fn may_hold_float(cx: &Ctx, t: TyId) -> bool {
    !matches!(
        cx.ty.kind(t),
        TyKind::Int(_)
            | TyKind::Bool
            | TyKind::Str
            | TyKind::Literal(
                hir::LitValue::Int(..) | hir::LitValue::Bool(_) | hir::LitValue::Str(_)
            )
    )
}
